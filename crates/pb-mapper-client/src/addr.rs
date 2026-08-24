//! Resolving a tunnel endpoint down to every address it names.
//!
//! The public entry points of both tunnel directions are generic over
//! `ToSocketAddrs`, so a caller can hand them a `&str`, a `SocketAddr`, or a
//! slice of them. Everything below those entry points works on a concrete
//! [`ResolvedAddrs`] instead: resolution happens once, at the boundary, and the
//! whole candidate list travels down to the dial loops rather than the first
//! address surviving and the rest being dropped.

use std::net::SocketAddr;

use pb_mapper_core::config::ResolvedAddrs;
use uni_stream::addr::ToSocketAddrs;

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
