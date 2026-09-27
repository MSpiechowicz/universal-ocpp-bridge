//! Enforce PostgreSQL backend wire limits before the driver's codec sees a frame header.
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::future::{FutureExt, Map};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::{
    Socket,
    tls::{ChannelBinding, MakeTlsConnect, TlsConnect, TlsStream},
};

// A single PostgreSQL frame (type byte plus length and body) and the bytes
// between ReadyForQuery messages must both be bounded. This includes startup.
const FRAME_LIMIT: usize = 1024 * 1024;
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const REJECTED: &str = "postgres backend response exceeds limit";

pub(crate) struct Guarded<S> {
    stream: S,
    header: [u8; 5],
    header_read: usize,
    header_sent: usize,
    body_left: usize,
    response_bytes: usize,
    ready: bool,
    rejected: bool,
}

impl<S> Guarded<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            header: [0; 5],
            header_read: 0,
            header_sent: 0,
            body_left: 0,
            response_bytes: 0,
            ready: false,
            rejected: false,
        }
    }

    fn frame_complete(&mut self) {
        if self.ready {
            self.response_bytes = 0;
        }
        self.header_read = 0;
        self.header_sent = 0;
        self.body_left = 0;
        self.ready = false;
    }

    fn fail(&mut self) -> Poll<io::Result<()>> {
        self.rejected = true;
        Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, REJECTED)))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Guarded<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.rejected {
            return self.fail();
        }
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        loop {
            if self.header_read < 5 {
                let this = &mut *self;
                let start = this.header_read;
                let mut part = ReadBuf::new(&mut this.header[start..]);
                match Pin::new(&mut this.stream).poll_read(cx, &mut part) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Ready(Ok(())) if part.filled().is_empty() => {
                        if start == 0 {
                            return Poll::Ready(Ok(()));
                        }
                        return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                    }
                    Poll::Ready(Ok(())) => this.header_read += part.filled().len(),
                }
                if self.header_read < 5 {
                    continue;
                }

                let length = u32::from_be_bytes(self.header[1..5].try_into().expect("fixed header"))
                    as usize;
                let frame_bytes = length.saturating_add(1);
                if length < 4
                    || frame_bytes > FRAME_LIMIT
                    || self.response_bytes.saturating_add(frame_bytes) > RESPONSE_LIMIT
                {
                    return self.fail();
                }
                self.response_bytes += frame_bytes;
                self.body_left = length - 4;
                self.ready = self.header[0] == b'Z' && length == 5;
            }

            if self.header_sent < 5 {
                let n = out.remaining().min(5 - self.header_sent);
                out.put_slice(&self.header[self.header_sent..self.header_sent + n]);
                self.header_sent += n;
                if self.header_sent == 5 && self.body_left == 0 {
                    self.frame_complete();
                }
                return Poll::Ready(Ok(()));
            }

            // Initialize only the portion the driver may receive. ReadBuf::take alone
            // does not propagate initialization metadata to the parent ReadBuf.
            let limit = out.remaining().min(self.body_left);
            let mut part = ReadBuf::new(out.initialize_unfilled_to(limit));
            match Pin::new(&mut self.stream).poll_read(cx, &mut part) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) => {
                    let n = part.filled().len();
                    if n == 0 {
                        return Poll::Ready(Err(io::ErrorKind::UnexpectedEof.into()));
                    }
                    out.advance(n);
                    self.body_left -= n;
                    if self.body_left == 0 {
                        self.frame_complete();
                    }
                    return Poll::Ready(Ok(()));
                }
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Guarded<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl<S: TlsStream + Unpin> TlsStream for Guarded<S> {
    fn channel_binding(&self) -> ChannelBinding {
        self.stream.channel_binding()
    }
}

pub(crate) struct GuardedTls<M>(pub(crate) M);

impl<M: MakeTlsConnect<Socket>> MakeTlsConnect<Socket> for GuardedTls<M> {
    type Stream = Guarded<M::Stream>;
    type TlsConnect = GuardedConnect<M::TlsConnect>;
    type Error = M::Error;

    fn make_tls_connect(&mut self, domain: &str) -> Result<Self::TlsConnect, Self::Error> {
        self.0.make_tls_connect(domain).map(GuardedConnect)
    }
}

pub(crate) struct GuardedConnect<T>(T);

type GuardMapper<S, E> = fn(Result<S, E>) -> Result<Guarded<S>, E>;

impl<T: TlsConnect<Socket>> TlsConnect<Socket> for GuardedConnect<T> {
    type Stream = Guarded<T::Stream>;
    type Error = T::Error;
    type Future = Map<T::Future, GuardMapper<T::Stream, T::Error>>;

    fn connect(self, stream: Socket) -> Self::Future {
        let guard: GuardMapper<T::Stream, T::Error> = |result| result.map(Guarded::new);
        self.0.connect(stream).map(guard)
    }
}
