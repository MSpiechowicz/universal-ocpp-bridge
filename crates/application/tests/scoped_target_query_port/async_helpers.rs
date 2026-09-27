use std::{
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

use uob_application::{RetainedEventItem, TargetPortError, TargetRetainedEventStream};

pub(super) fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

pub(super) fn poll_stream<E>(
    stream: &mut TargetRetainedEventStream<E>,
) -> Poll<Option<Result<RetainedEventItem<E>, TargetPortError>>> {
    let mut context = Context::from_waker(Waker::noop());
    stream.as_mut().poll_event(&mut context)
}
