//! The race that finds a path: probing a peer's candidates and dialing
//! those that answer.
use super::Network;
use crate::{
    diagnostics::LanEvidence,
    direct::{
        conn::{DirectConn, QuicGuard},
        identity::SERVER_NAME,
        lan::Assessment,
        link::first_answers,
        tcp,
    },
};
use pb_mapper_protocol::direct::SessionGrant;
use std::{io, net::SocketAddr, time::Duration};
use tokio::{task::JoinSet, time::Instant};

/// How often a peer's candidates are all probed.
const PROBE_INTERVAL: Duration = Duration::from_millis(120);
/// QUIC handshakes one setup starts at most: an attempt is bounded work, and
/// the setup deadline ends the search anyway.
const MAX_DIALS_PER_SETUP: usize = 16;

/// How long a path that is not on the LAN waits after its first answer,
/// with LAN-first on, once the LAN is known to be there (by strong evidence,
/// or because a path on it answered): a LAN handshake that finishes in the
/// meantime saves a handshake to every other path.
const LAN_HEAD_START: Duration = Duration::from_millis(150);
/// When paths on the LAN are probed again after the first round, with
/// LAN-first on and any evidence of one: a first probe lost to Wi-Fi, or
/// dropped by a firewall the peer's own probe had not opened yet, costs
/// 20 ms, not 120.
const LAN_REPROBES: [Duration; 2] = [Duration::from_millis(20), Duration::from_millis(50)];
const _: () = assert!(
    LAN_REPROBES[1].as_nanos() < PROBE_INTERVAL.as_nanos(),
    "a LAN re-probe lands before the next round"
);

/// How the search for a path went, once one connected.
pub(in crate::direct) struct Dialed {
    pub probed: u32,
    pub answered: u32,
    pub handshakes: u32,
    /// From the start of the search to the first answer, and the first on a
    /// local network.
    pub first_pong: Option<Duration>,
    pub lan_first_pong: Option<Duration>,
    /// The winning handshake.
    pub handshake: Duration,
}

impl Network {
    /// How a peer's candidates are probed: all of them every
    /// [`PROBE_INTERVAL`]. With LAN-first on, those on a local network go
    /// first, and on evidence of one again shortly after the first round;
    /// off, they keep the peer's order and nothing is probed again early.
    fn probing(candidates: &[SocketAddr], lan: &Assessment) -> (Vec<SocketAddr>, Vec<SocketAddr>) {
        let ordered = lan.order(candidates);
        let reprobed = ordered
            .iter()
            .copied()
            .filter(|addr| lan.prefers() && lan.is_lan(*addr))
            .collect();
        (ordered, reprobed)
    }
    /// Keep probing a peer for the provider, which is dialed rather than
    /// dialing: its probes open its own NAT and firewall for the consumer's,
    /// and answer the sources the consumer's probes come from.
    pub async fn keep_probing(&self, grant: &SessionGrant, lan: &Assessment) {
        let id = grant.id;
        let (ordered, reprobed) = Self::probing(&grant.consumer.candidates, lan);
        let wake = self.wake(&id);
        let started = Instant::now();
        let mut reprobes = reprobe_times(started, lan.preferred(), &reprobed);
        let mut probes = tokio::time::interval(PROBE_INTERVAL);
        loop {
            let reprobe = reprobes.first().copied();
            tokio::select! {
                _ = probes.tick() => {
                    self.probe(id, &ordered);
                }
                () = sleep_until_some(reprobe) => {
                    reprobes.remove(0);
                    self.probe(id, &reprobed);
                }
                () = wake.notified() => {
                    self.probe(id, &[]);
                }
            }
        }
    }
    /// Dial the provider over each path as soon as it answers a probe, until
    /// one handshake completes. Only paths that answered are raced: blind
    /// QUIC flights to stale interface addresses can fill one shared UDP
    /// socket's send queue and stall healthy sessions. TCP needs no probe,
    /// its connect being its own test: `tcp`'s attempts run from the start,
    /// and with LAN-first on every QUIC path waits out their head start,
    /// then those off the LAN wait for QUIC on it as after their answer.
    pub async fn connect(
        &self,
        grant: &SessionGrant,
        deadline: Instant,
        lan: &Assessment,
        tcp: &mut tcp::Race,
    ) -> io::Result<(DirectConn, Dialed)> {
        let id = grant.id;
        let started = Instant::now();
        let config = self.identity.client(&grant.provider.certificate)?;
        let (ordered, reprobed) = Self::probing(&grant.provider.candidates, lan);
        let probed = self.reachable(&ordered);
        let wake = self.wake(&id);
        let mut reprobes = reprobe_times(started, lan.preferred(), &reprobed);
        let mut attempts = JoinSet::new();
        let mut tried = Vec::new();
        // LAN handshakes started and failed, and whether the system refused
        // to send to a LAN address: either way the head start is over.
        let (mut lan_dialed, mut lan_failed, mut lan_refused) = (0, 0, false);
        let mut probe = tokio::time::interval(PROBE_INTERVAL);
        let dialed = |handshakes: u32, handshake| {
            let learned = self.learned(&id);
            let (first_pong, lan_first_pong) = first_answers(&learned, started, lan);
            Dialed {
                probed,
                answered: learned.len() as u32,
                handshakes,
                first_pong,
                lan_first_pong,
                handshake,
            }
        };
        loop {
            let now = Instant::now();
            // While TCP has its head start, no QUIC path is dialed; after it,
            // QUIC on the LAN still goes before the paths off it.
            let tcp_holds = tcp.holds(lan.prefers(), now);
            let tcp_released = tcp.released(lan.prefers());
            let mut held: Option<Instant> = tcp_holds;
            let learned = self.learned(&id);
            // A LAN path that answered an authenticated probe is proof of
            // the LAN, where the offer was only evidence of it.
            let hold = lan.holds(learned.iter().any(|(addr, _)| lan.is_lan(*addr)));
            for (addr, answered) in learned {
                if tried.contains(&addr)
                    || tried.len() >= MAX_DIALS_PER_SETUP
                    || tcp_holds.is_some()
                {
                    continue;
                }
                let on_lan = lan.is_lan(addr);
                let settled = lan_refused || (lan_dialed > 0 && lan_failed == lan_dialed);
                let answered = tcp_released.map_or(answered, |released| answered.max(released));
                let since = now.saturating_duration_since(answered);
                if !dial_now(hold, on_lan, since, settled) {
                    let at = answered + LAN_HEAD_START;
                    held = Some(held.map_or(at, |held| held.min(at)));
                    continue;
                }
                tried.push(addr);
                if let Some(transport) = self.transport_for(addr.is_ipv4())
                    && let Ok(connecting) =
                        transport
                            .endpoint
                            .connect_with(config.clone(), addr, SERVER_NAME)
                {
                    lan_dialed += u32::from(on_lan);
                    attempts.spawn(async move {
                        let dialed = Instant::now();
                        (addr, connecting.await, dialed.elapsed())
                    });
                }
            }
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "direct path timed out"));
                }
                _ = probe.tick() => {
                    let refused = self.probe(id, &ordered);
                    lan_refused |= refused.iter().any(|addr| lan.is_lan(*addr));
                }
                () = sleep_until_some(reprobes.first().copied()) => {
                    reprobes.remove(0);
                    let refused = self.probe(id, &reprobed);
                    lan_refused |= refused.iter().any(|addr| lan.is_lan(*addr));
                }
                // A source owed a challenge gets it now; a path that answered
                // is dialed above.
                () = wake.notified() => {
                    self.probe(id, &[]);
                }
                () = sleep_until_some(held) => {}
                // A failed attempt is noted; the last one to fail ends the
                // head start, which the next round sees.
                result = tcp.next(), if tcp.pending() => match result {
                    Ok(won) => {
                        let dialed = dialed(tried.len() as u32 + tcp.dialed(), won.handshake);
                        let conn = DirectConn::Tcp { stream: won.stream, rtt: won.rtt };
                        return Ok((conn, dialed));
                    }
                    Err(miss) => tcp.record(miss),
                },
                Some(result) = attempts.join_next(), if !attempts.is_empty() => {
                    match result {
                        Ok((_, Ok(connection), handshake)) => {
                            let dialed = dialed(tried.len() as u32 + tcp.dialed(), handshake);
                            let connection = QuicGuard(connection);
                            let (send, recv) = connection.0.open_bi().await.map_err(io::Error::other)?;
                            return Ok((DirectConn::Quic { connection, send, recv }, dialed));
                        }
                        // A systematic identity or config mismatch must be
                        // distinguishable from an unreachable path.
                        Ok((addr, Err(error), _)) => {
                            lan_failed += u32::from(lan.is_lan(addr));
                            tracing::debug!(%error, %addr, "direct QUIC attempt failed");
                        }
                        Err(error) => tracing::debug!(%error, "direct QUIC attempt aborted"),
                    }
                }
            }
        }
    }
}
/// Whether a path that answered `since` ago may be dialed now. Only while
/// the LAN [`holds`](Assessment::holds) other paths back is a path off it
/// held, and only for [`LAN_HEAD_START`] or until the LAN has failed.
fn dial_now(hold: bool, on_lan: bool, since: Duration, lan_failed: bool) -> bool {
    on_lan || lan_failed || since >= LAN_HEAD_START || !hold
}

/// When LAN paths are probed again, on any evidence of a shared network
/// that this end acts on.
fn reprobe_times(
    started: Instant,
    evidence: Option<LanEvidence>,
    lan: &[SocketAddr],
) -> Vec<Instant> {
    if evidence.is_none() || lan.is_empty() {
        return Vec::new();
    }
    LAN_REPROBES.iter().map(|after| started + *after).collect()
}

async fn sleep_until_some(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::Arc;

    #[test]
    fn only_a_lan_known_to_be_there_holds_other_paths_back_and_never_for_long() {
        let assessed = |evidence, prefer| {
            let mut lan = Assessment::new(Arc::default(), &[], prefer);
            lan.evidence = evidence;
            lan
        };
        let strong = assessed(Some(LanEvidence::PublicIpSubnet), true);
        let weak = assessed(Some(LanEvidence::Subnet), true);
        let none = assessed(None, true);
        let soon = Duration::from_millis(10);
        // No evidence dials whatever answers at once; so does weak evidence,
        // a private subnet that may merely collide, while no LAN path has
        // answered.
        assert!(dial_now(none.holds(false), false, soon, false));
        assert!(dial_now(weak.holds(false), false, soon, false));
        // Strong evidence: the LAN at once, the rest after the head start,
        // or as soon as the LAN has failed.
        let hold = strong.holds(false);
        assert!(dial_now(hold, true, Duration::ZERO, false));
        assert!(!dial_now(hold, false, soon, false));
        assert!(dial_now(hold, false, LAN_HEAD_START, false));
        assert!(dial_now(hold, false, soon, true));
        // A LAN path's answer proves the LAN, whatever the offer showed.
        assert!(!dial_now(weak.holds(true), false, soon, false));
        assert!(!dial_now(none.holds(true), false, soon, false));
        // LAN-first off: nothing waits, and nothing is probed again early.
        let off = assessed(Some(LanEvidence::PublicIpSubnet), false);
        assert!(dial_now(off.holds(true), false, soon, false));
        let lan = [SocketAddr::from(([192, 168, 1, 2], 4000))];
        assert!(reprobe_times(Instant::now(), off.preferred(), &lan).is_empty());
        assert_eq!(
            reprobe_times(Instant::now(), strong.preferred(), &lan).len(),
            LAN_REPROBES.len()
        );
    }
}
