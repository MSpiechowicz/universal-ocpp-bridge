//! Notifications are emitted by the owning service loop, never a detached timer.
use std::{io, time::Duration};

pub(crate) struct Notifier {
    #[cfg(unix)]
    socket: Option<std::os::unix::net::UnixDatagram>,
    pub(crate) interval: Option<Duration>,
}

impl Notifier {
    pub(crate) fn from_environment() -> io::Result<Self> {
        let address = std::env::var_os("NOTIFY_SOCKET");
        let interval = watchdog_interval(
            std::env::var("WATCHDOG_USEC").ok().as_deref(),
            std::env::var("WATCHDOG_PID").ok().as_deref(),
            std::process::id(),
        )?;
        if interval.is_some() && address.is_none() {
            return Err(io::Error::other("watchdog requires a notification socket"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::{ffi::OsStrExt, net::UnixDatagram};
            let socket = address
                .map(|address| {
                    let socket = UnixDatagram::unbound()?;
                    socket.set_nonblocking(true)?;
                    let bytes = address.as_bytes();
                    if bytes.starts_with(b"/") {
                        socket.connect(address)?;
                    } else {
                        #[cfg(target_os = "linux")]
                        {
                            use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};
                            let name = bytes
                                .strip_prefix(b"@")
                                .filter(|name| !name.is_empty())
                                .ok_or_else(|| io::Error::other("invalid notification socket"))?;
                            socket.connect_addr(&SocketAddr::from_abstract_name(name)?)?;
                        }
                        #[cfg(not(target_os = "linux"))]
                        return Err(io::Error::other("unsupported notification socket"));
                    }
                    Ok::<_, io::Error>(socket)
                })
                .transpose()?;
            Ok(Self { socket, interval })
        }
        #[cfg(not(unix))]
        {
            if address.is_some() {
                return Err(io::Error::other("systemd notifications require Unix"));
            }
            Ok(Self { interval })
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        #[cfg(unix)]
        {
            self.socket.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub(crate) fn send(&self, message: &str) -> io::Result<()> {
        #[cfg(unix)]
        if let Some(socket) = &self.socket
            && socket.send(message.as_bytes())? != message.len()
        {
            return Err(io::Error::other("incomplete systemd notification"));
        }
        Ok(())
    }
}

fn watchdog_interval(
    usec: Option<&str>,
    pid: Option<&str>,
    current: u32,
) -> io::Result<Option<Duration>> {
    let invalid = || io::Error::other("invalid systemd watchdog configuration");
    if let Some(pid) = pid
        && pid.parse::<u32>().map_err(|_| invalid())? != current
    {
        return Ok(None);
    }
    usec.map(|value| {
        let micros = value.parse::<u64>().map_err(|_| invalid())?;
        // Avoid a zero interval and unreasonable polling/overflow from malformed configuration.
        if !(100_000..=300_000_000).contains(&micros) {
            return Err(invalid());
        }
        Ok(Duration::from_micros(micros / 2))
    })
    .transpose()
}

/// A fresh worker round trip is required every time; one pending probe at most.
pub(crate) async fn progress(
    probe: impl Future<Output = io::Result<()>>,
    interval: Duration,
) -> io::Result<()> {
    tokio::time::sleep(interval).await;
    tokio::time::timeout(interval, probe)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "storage progress stalled"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_environment_is_scoped_and_bounded() {
        assert_eq!(
            watchdog_interval(Some("1000000"), Some("42"), 42).unwrap(),
            Some(Duration::from_millis(500))
        );
        assert!(
            watchdog_interval(Some("1000000"), Some("43"), 42)
                .unwrap()
                .is_none()
        );
        assert!(watchdog_interval(None, None, 42).unwrap().is_none());
        for invalid in ["0", "1", "-1", "bad", "18446744073709551615"] {
            assert!(watchdog_interval(Some(invalid), None, 42).is_err());
        }
    }

    #[tokio::test]
    async fn unrelated_timer_cannot_complete_a_stalled_storage_probe() {
        let interval = Duration::from_millis(10);
        let stalled = progress(std::future::pending(), interval);
        tokio::pin!(stalled);
        let mut timer = tokio::time::interval(Duration::from_millis(1));
        let mut ticks = 0;
        loop {
            tokio::select! {
                result = &mut stalled => {
                    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
                    assert!(ticks > 1);
                    break;
                }
                _ = timer.tick() => ticks += 1,
            }
        }
        progress(async { Ok(()) }, interval).await.unwrap();
    }
}
