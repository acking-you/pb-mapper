//! What a committed session's path is, as both ends report it.
use super::{
    conn::DirectConn,
    endpoint::Discovered,
    lan::{self, Assessment},
};
use crate::diagnostics::{
    CandidateCounts, Diagnostics, DirectLink, LanEvidence, LinkKind, LinkStats, LinkVia,
    SetupTimings,
};
use pb_mapper_protocol::direct::{AddressFamily, LinkSummary};
use std::{io, net::SocketAddr, time::Duration};
use tokio::time::Instant;

/// The figures from before the search: discovery's, with how STUN went.
pub(super) fn stun_timings(timings: SetupTimings, discovered: &Discovered) -> SetupTimings {
    SetupTimings {
        stun_ms: discovered.stun_ms,
        stun: discovered.stun,
        ..timings
    }
}

/// How long after `since` a path first answered a probe, and a path on the
/// LAN, from what the endpoint `learned`.
pub(super) fn first_answers(
    learned: &[(SocketAddr, Instant)],
    since: Instant,
    lan: &Assessment,
) -> (Option<Duration>, Option<Duration>) {
    let first = |lan_only: bool| {
        learned
            .iter()
            .filter(|(addr, _)| !lan_only || lan.is_lan(*addr))
            .map(|(_, at)| at.saturating_duration_since(since))
            .min()
    };
    (first(false), first(true))
}

/// The candidates each end offered.
pub(super) fn counts(
    discovered: &Discovered,
    lan: &Assessment,
    peer: &[SocketAddr],
) -> CandidateCounts {
    let (local, local_lan, truncated) = discovered.counts;
    CandidateCounts {
        local,
        local_lan,
        truncated,
        peer: peer.len() as u32,
        peer_lan: peer.iter().filter(|addr| lan.is_lan(**addr)).count() as u32,
        ..CandidateCounts::default()
    }
}

/// What a committed session's path is, as this end sees it. `offered` is
/// what the peer offered.
pub(super) fn direct_link(
    conn: &DirectConn,
    lan: &Assessment,
    offered: &[SocketAddr],
    timings: SetupTimings,
    candidates: CandidateCounts,
) -> io::Result<DirectLink> {
    let peer = conn.remote_address()?;
    let local_ip = conn.local_ip();
    let interface = local_ip.and_then(|ip| lan.local.interfaces.local(ip));
    Ok(DirectLink {
        kind: lan.kind(local_ip, peer.ip()),
        family: AddressFamily::of(peer.ip()),
        via: via(peer, offered),
        transport: conn.transport(),
        peer,
        local_ip,
        interface: interface.map(|interface| interface.name.clone()),
        tier: interface.map(|interface| interface.tier),
        same_lan: lan.evidence,
        timings,
        candidates,
        stats: LinkStats::default(),
    })
}

/// How the peer address was learned. A peer offers its router mapping, then
/// what STUN saw, then its interfaces: two public IPv4 addresses in front
/// mean the first is the mapping.
fn via(peer: SocketAddr, offered: &[SocketAddr]) -> LinkVia {
    let Some(index) = offered.iter().position(|addr| *addr == peer) else {
        return LinkVia::PeerReflexive;
    };
    let public_v4 = |addr: &SocketAddr| addr.is_ipv4() && lan::is_global(addr.ip());
    if !public_v4(&peer) {
        LinkVia::Host
    } else if index == 0 && offered.get(1).is_some_and(public_v4) {
        LinkVia::Mapped
    } else {
        LinkVia::Public
    }
}

/// A LAN the peer's offer gave evidence of did not carry the session: `took`
/// is the path that did, or none when the relay will.
pub(super) fn lan_missed(diagnostics: &Diagnostics, lan: &Assessment, took: Option<LinkKind>) {
    if let Some(evidence) = lan.evidence {
        diagnostics.lan_missed(evidence, took);
    }
}

/// What a report says of a path: no address.
pub(super) fn summary(link: &DirectLink) -> LinkSummary {
    LinkSummary {
        kind: link.kind,
        family: link.family,
        transport: link.transport,
    }
}

/// The line that says which way a session went, which the app's Logs page
/// shows. It carries the figures a LAN-first A/B compares: the setup time,
/// how soon a LAN address answered (absent when none did) and the handshakes
/// this end started (none on the publishing end, which is dialed).
pub(super) fn log_path(service: &str, link: &DirectLink) {
    tracing::info!(
        event = "session_path_selected",
        path = "direct",
        service = %service,
        peer = %link.peer,
        kind = link.kind.as_str(),
        family = link.family.as_str(),
        transport = link.transport.as_str(),
        local_ip = %link.local_ip.map_or_else(|| "-".to_string(), |ip| ip.to_string()),
        evidence = link.same_lan.map_or("none", LanEvidence::as_str),
        setup_ms = link.timings.setup_ms,
        lan_first_pong_ms = link.timings.lan_first_pong_ms,
        // Only the dialing end makes handshakes; the dialed end has none to count.
        handshakes = link.timings.handshake_ms.map(|_| link.candidates.handshakes),
    );
}
