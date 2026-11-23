use std::{io, time::Duration};

const INITIAL_RETRY: Duration = Duration::from_millis(100);
const MAX_RETRY: Duration = Duration::from_secs(2);

pub(crate) async fn retry_network_errors<T>(
    mut recv: impl AsyncFnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut delay = INITIAL_RETRY;
    loop {
        match recv().await {
            Err(error) if temporary_network_error(&error) => {
                if delay == INITIAL_RETRY {
                    tracing::debug!(%error, "Waiting for the UDP route to recover");
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(MAX_RETRY);
            }
            result => return result,
        }
    }
}

fn temporary_network_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::AddrNotAvailable
            | io::ErrorKind::HostUnreachable
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::NetworkUnreachable
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Instant, timeout};

    #[tokio::test(start_paused = true)]
    async fn receives_next_datagram_after_address_and_route_errors() {
        let started = Instant::now();
        let mut errors = [
            io::ErrorKind::AddrNotAvailable,
            io::ErrorKind::HostUnreachable,
            io::ErrorKind::NetworkDown,
            io::ErrorKind::NetworkUnreachable,
            io::ErrorKind::HostUnreachable,
            io::ErrorKind::HostUnreachable,
        ]
        .into_iter();
        let packet = retry_network_errors(async || match errors.next() {
            Some(kind) => Err(io::Error::from(kind)),
            None => Ok(b"peer response"),
        })
        .await
        .unwrap();
        assert_eq!(packet, b"peer response");
        assert_eq!(started.elapsed(), Duration::from_millis(5100));
    }

    #[tokio::test(start_paused = true)]
    async fn keeps_permanent_errors_and_allows_cancelling_during_backoff() {
        let started = Instant::now();
        let error = retry_network_errors(async || Err::<(), _>(io::ErrorKind::InvalidInput.into()))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(started.elapsed().is_zero());

        let mut attempts = 0;
        let result = timeout(
            Duration::from_millis(50),
            retry_network_errors(async || {
                attempts += 1;
                Err::<(), _>(io::ErrorKind::HostUnreachable.into())
            }),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recognizes_the_darwin_errors_recorded_by_qa() {
        for code in [libc::EADDRNOTAVAIL, libc::EHOSTUNREACH] {
            assert!(temporary_network_error(&io::Error::from_raw_os_error(code)));
        }
    }
}
