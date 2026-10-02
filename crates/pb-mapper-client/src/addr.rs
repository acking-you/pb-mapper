//! Resolving a tunnel endpoint down to every address it names.
//!
//! The public entry points of both tunnel directions are generic over
//! `ToSocketAddrs`, so a caller can hand them a `&str`, a `SocketAddr`, or a
//! slice of them. Everything below those entry points works on a concrete
//! [`ResolvedAddrs`] instead: resolution happens once, at the boundary, and the
//! whole candidate list travels down to the dial loops rather than the first
//! address surviving and the rest being dropped.

use std::net::SocketAddr;
use std::time::Duration;

use pb_mapper_core::config::ResolvedAddrs;
use uni_stream::addr::ToSocketAddrs;

/// Race a small number of TCP candidates so a blackholed first address cannot
/// hide a working IPv4/IPv6 alternative. Callers retain their overall deadline.
pub(crate) async fn connect_tcp(addrs: &ResolvedAddrs) -> std::io::Result<tokio::net::TcpStream> {
    race_candidates(addrs.as_slice(), |addr| {
        tokio::net::TcpStream::connect(addr)
    })
    .await
}

async fn race_candidates<T, F, Fut>(addrs: &[SocketAddr], dial: F) -> std::io::Result<T>
where
    T: Send + 'static,
    F: Fn(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>> + Send + 'static,
{
    // A single address keeps the caller's adaptive setup budget.
    if let [addr] = addrs {
        return dial(*addr).await;
    }
    let mut remaining = addrs.iter();
    let mut attempts = tokio::task::JoinSet::new();
    let mut next_attempt = tokio::time::Instant::now();
    let mut last_error = std::io::Error::new(std::io::ErrorKind::NotFound, "no TCP addresses");
    loop {
        if attempts.is_empty()
            || (attempts.len() < 2 && tokio::time::Instant::now() >= next_attempt)
        {
            if let Some(addr) = remaining.next() {
                let future = dial(*addr);
                attempts.spawn(async move {
                    tokio::time::timeout(Duration::from_secs(2), future)
                        .await
                        .unwrap_or_else(|_| Err(std::io::ErrorKind::TimedOut.into()))
                });
                next_attempt = tokio::time::Instant::now() + Duration::from_millis(250);
            } else if attempts.is_empty() {
                return Err(last_error);
            }
        }
        tokio::select! {
            Some(result) = attempts.join_next() => match result {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(error)) => last_error = error,
                Err(error) => last_error = std::io::Error::other(error),
            },
            () = tokio::time::sleep_until(next_attempt), if attempts.len() < 2 && remaining.len() > 0 => {}
        }
    }
}

/// Resolve `addr` to every address it names.
///
/// Fails when nothing resolved: a tunnel with no candidate to dial cannot start,
/// and saying so here is clearer than a connect error against an address the
/// caller never supplied.
pub(crate) async fn resolve_all<A: ToSocketAddrs>(addr: A) -> std::io::Result<ResolvedAddrs> {
    let candidates: Vec<SocketAddr> = addr.to_socket_addrs().await?.collect();
    ResolvedAddrs::from_candidates(candidates).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "could not resolve to any addresses",
        )
    })
}

/// Resolve both ends of a tunnel, logging which end failed if either does.
///
/// This is where a generic entry point becomes the concrete candidate lists that
/// everything below it works on. Resolution happens once, here: the retry loops
/// reconnect to the addresses they were given rather than repeating a lookup that
/// already succeeded.
pub(crate) async fn resolve_tunnel_ends<A: ToSocketAddrs>(
    local_addr: A,
    remote_addr: A,
) -> Option<(ResolvedAddrs, ResolvedAddrs)> {
    let local_addr = match resolve_all(local_addr).await {
        Ok(addrs) => addrs,
        Err(e) => {
            tracing::error!("parse local addr failed: {e}");
            return None;
        }
    };
    let remote_addr = match resolve_all(remote_addr).await {
        Ok(addrs) => addrs,
        Err(e) => {
            tracing::error!("parse remote addr failed: {e}");
            return None;
        }
    };
    Some((local_addr, remote_addr))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn blackholed_first_address_does_not_hide_working_candidate() {
        let active = Arc::new(AtomicUsize::new(0));
        let candidates = [
            "127.0.0.1:1".parse().unwrap(),
            "127.0.0.1:2".parse().unwrap(),
        ];
        let started = tokio::time::Instant::now();
        let result = race_candidates(&candidates, |addr| {
            let active = active.clone();
            async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _guard = Active(active);
                if addr.port() == 1 {
                    std::future::pending::<()>().await;
                }
                Ok(addr)
            }
        })
        .await
        .unwrap();
        assert_eq!(result, candidates[1]);
        assert_eq!(started.elapsed(), Duration::from_millis(250));
        tokio::task::yield_now().await;
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn candidate_racing_bounds_resources_and_reaches_later_addresses() {
        let active = Arc::new(AtomicUsize::new(0));
        let candidates = (1..=5)
            .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
            .collect::<Vec<_>>();
        let result = race_candidates(&candidates, |addr| {
            let active = active.clone();
            async move {
                assert!(active.fetch_add(1, Ordering::SeqCst) < 2);
                let _guard = Active(active);
                if addr.port() < 5 {
                    std::future::pending::<()>().await;
                }
                Ok(addr)
            }
        })
        .await
        .unwrap();
        assert_eq!(result.port(), 5);
        tokio::task::yield_now().await;
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_a_dial_reaps_all_candidate_tasks() {
        let active = Arc::new(AtomicUsize::new(0));
        let candidates = [
            "127.0.0.1:1".parse().unwrap(),
            "127.0.0.1:2".parse().unwrap(),
        ];
        let dial = race_candidates(&candidates, |_| {
            let active = active.clone();
            async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _guard = Active(active);
                std::future::pending::<std::io::Result<()>>().await
            }
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(500), dial)
                .await
                .is_err()
        );
        tokio::task::yield_now().await;
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
}
