//! Diagnostics and metrics as JS objects, converted from the SDK's.

use napi_derive::napi;
use pb_mapper_client::sdk;

use crate::convert::number;

/// A tunnel's gateway port mapping after its latest change.
#[napi(object)]
pub struct JsPortMappingStatus {
    /// `waiting_for_direct`, `discovering`, `gateway_found`, `discovered`,
    /// `requesting`, `active`, `renewing`, `expired`, `retrying`,
    /// `network_changed`, `releasing`, `released`, `release_sent`, `failed`,
    /// `release_failed`, `cancelled` or `stopped`.
    pub stage: String,
    pub network_generation: f64,
    pub local_port: Option<u32>,
    pub gateway: Option<String>,
    /// `pcp`, `nat_pmp` or `upnp`.
    pub protocol: Option<String>,
    pub available_protocols: Vec<String>,
    pub external_addr: Option<String>,
    /// Whether a lease is usable now, renewal pending or failed included.
    pub active: bool,
    pub lease_lifetime_ms: Option<f64>,
    pub error: Option<String>,
}

impl From<sdk::PortMappingStatus> for JsPortMappingStatus {
    fn from(value: sdk::PortMappingStatus) -> Self {
        Self {
            stage: value.stage.as_str().into(),
            network_generation: number(value.network_generation),
            local_port: value.local_port.map(u32::from),
            gateway: value.gateway.map(|v| v.to_string()),
            protocol: value.protocol.map(|v| v.as_str().into()),
            available_protocols: value
                .available_protocols
                .into_iter()
                .map(|v| v.as_str().into())
                .collect(),
            external_addr: value.external_addr.map(|v| v.to_string()),
            active: value.active,
            lease_lifetime_ms: value.lease_lifetime_ms.map(number),
            error: value.error,
        }
    }
}

#[napi(object)]
pub struct JsPortMappingDiagnostics {
    pub current: JsPortMappingStatus,
    pub attempts: f64,
    pub renewals: f64,
    pub failures: f64,
}

#[napi(object)]
pub struct JsRelayDiagnostics {
    pub transport: String,
    pub connections_created: f64,
    pub streams_opened: f64,
    pub streams_reused: f64,
    pub last_error: Option<String>,
    pub connections: Vec<JsQuicConnectionDiagnostics>,
    /// The smoothed round trip of every tunnel's control exchanges with
    /// this relay over this carrier, the relay's handling included.
    pub control_rtt_ms: Option<f64>,
}

#[napi(object)]
pub struct JsQuicConnectionDiagnostics {
    pub connection_id: f64,
    pub peer_addr: String,
    /// `false` once the pool has retired it: it stays open only for the
    /// streams already on it.
    pub pooled: bool,
    pub rtt_us: f64,
    pub min_rtt_us: f64,
    pub congestion_window: f64,
    pub mtu: u32,
    pub sent_packets: f64,
    pub lost_packets: f64,
    pub congestion_events: f64,
    /// UDP bytes each way, and lost, of every tunnel on the connection.
    pub sent_bytes: f64,
    pub received_bytes: f64,
    pub lost_bytes: f64,
    /// The congestion controller's estimate of the path, in bits per second.
    pub bandwidth_bps: Option<f64>,
}

impl From<sdk::RelayDiagnostics> for JsRelayDiagnostics {
    fn from(value: sdk::RelayDiagnostics) -> Self {
        Self {
            transport: value.transport.as_str().into(),
            connections_created: number(value.connections_created),
            streams_opened: number(value.streams_opened),
            streams_reused: number(value.streams_reused),
            last_error: value.last_error,
            connections: value
                .connections
                .into_iter()
                .map(|connection| JsQuicConnectionDiagnostics {
                    connection_id: number(connection.connection_id),
                    peer_addr: connection.peer_addr.to_string(),
                    pooled: connection.pooled,
                    rtt_us: number(connection.rtt_us),
                    min_rtt_us: number(connection.min_rtt_us),
                    congestion_window: number(connection.congestion_window),
                    mtu: connection.mtu.into(),
                    sent_packets: number(connection.sent_packets),
                    lost_packets: number(connection.lost_packets),
                    congestion_events: number(connection.congestion_events),
                    sent_bytes: number(connection.sent_bytes),
                    received_bytes: number(connection.received_bytes),
                    lost_bytes: number(connection.lost_bytes),
                    bandwidth_bps: connection.bandwidth_bps.map(number),
                })
                .collect(),
            control_rtt_ms: value.control_rtt_ms.map(number),
        }
    }
}

/// Application bytes, after decryption and without framing: `sent` from
/// this end's local side, `received` back.
#[napi(object)]
pub struct JsByteCount {
    pub sent: f64,
    pub received: f64,
}

impl From<sdk::ByteCount> for JsByteCount {
    fn from(value: sdk::ByteCount) -> Self {
        Self {
            sent: number(value.sent),
            received: number(value.received),
        }
    }
}

/// A tunnel's application bytes by path, over its life; `direct` includes
/// `lan`.
#[napi(object)]
pub struct JsPathTraffic {
    pub relay: JsByteCount,
    pub direct: JsByteCount,
    pub lan: JsByteCount,
}

impl From<sdk::PathTraffic> for JsPathTraffic {
    fn from(value: sdk::PathTraffic) -> Self {
        Self {
            relay: value.relay.into(),
            direct: value.direct.into(),
            lan: value.lan.into(),
        }
    }
}

/// A tunnel's sessions on each path: open now, and since it started.
#[napi(object)]
pub struct JsSessionCounts {
    pub active_relay: f64,
    pub active_direct: f64,
    pub active_lan: f64,
    pub relay: f64,
    pub direct: f64,
    pub lan: f64,
}

/// How a tunnel's relay is doing, as its own attempts and probes see it.
#[napi(object)]
pub struct JsRelayHealth {
    /// `tcp` or `quic`.
    pub transport: String,
    /// The relay attempt's phase, as `lastAttemptPhase` names it.
    pub phase: String,
    pub consecutive_failures: f64,
    /// The smoothed round trip of the tunnel's control exchanges with the
    /// relay, the relay's handling included.
    pub rtt_ms: Option<f64>,
    pub last_success_age_ms: Option<f64>,
}

/// The newest direct session open now, with QUIC's view of it.
#[napi(object)]
pub struct JsLinkSample {
    /// `lan`, `internet` or `private`.
    pub kind: String,
    /// `ipv4` or `ipv6`.
    pub family: String,
    /// `host`, `public`, `mapped` or `peer_reflexive`.
    pub via: String,
    /// `quic`, or `tcp` within a local network. Over TCP the round trips are
    /// the handshake's, and the rest is zero.
    pub transport: String,
    pub peer: String,
    pub local_ip: Option<String>,
    /// `ipv6_prefix`, `public_ip_subnet` or `subnet`.
    pub same_lan: Option<String>,
    pub setup_ms: f64,
    pub rtt_us: f64,
    pub min_rtt_us: f64,
    pub congestion_window: f64,
    pub mtu: u32,
    pub sent_packets: f64,
    pub lost_packets: f64,
    pub congestion_events: f64,
    /// UDP bytes on the wire, QUIC's overhead included.
    pub sent_bytes: f64,
    pub received_bytes: f64,
    pub lost_bytes: f64,
    pub bandwidth_bps: Option<f64>,
}

/// Setups that found the peer on a local network, and those that ended
/// elsewhere.
#[napi(object)]
pub struct JsLanCounts {
    pub detected: f64,
    pub missed: f64,
}

/// What a running tunnel keeps measuring, sampled at once.
#[napi(object)]
pub struct JsTunnelMetrics {
    pub uptime_ms: f64,
    /// The `seq` of the tunnel's latest event.
    pub event_seq: f64,
    pub sessions: JsSessionCounts,
    pub traffic: JsPathTraffic,
    pub relay: JsRelayHealth,
    pub link: Option<JsLinkSample>,
    pub lan: JsLanCounts,
    /// How long new sessions still go straight to the relay after a
    /// fallback, and how many have.
    pub direct_cooldown_ms: Option<f64>,
    pub cooldown_skips: f64,
    /// Whether direct setups try a shared local network first.
    pub lan_first: bool,
}

impl From<sdk::TunnelMetrics> for JsTunnelMetrics {
    fn from(value: sdk::TunnelMetrics) -> Self {
        let sessions = value.sessions;
        let relay = value.relay;
        Self {
            uptime_ms: number(value.uptime_ms),
            event_seq: number(value.event_seq),
            sessions: JsSessionCounts {
                active_relay: number(sessions.active_relay),
                active_direct: number(sessions.active_direct),
                active_lan: number(sessions.active_lan),
                relay: number(sessions.relay),
                direct: number(sessions.direct),
                lan: number(sessions.lan),
            },
            traffic: value.traffic.into(),
            relay: JsRelayHealth {
                transport: relay.transport.as_str().into(),
                phase: relay.phase.as_str().into(),
                consecutive_failures: number(relay.consecutive_failures),
                rtt_ms: relay.rtt_ms.map(number),
                last_success_age_ms: relay.last_success_age_ms.map(number),
            },
            link: value.link.map(|link| JsLinkSample {
                kind: link.kind.as_str().into(),
                family: link.family.as_str().into(),
                via: link.via.as_str().into(),
                transport: link.transport.as_str().into(),
                peer: link.peer.to_string(),
                local_ip: link.local_ip.map(|ip| ip.to_string()),
                same_lan: link.same_lan.map(|evidence| evidence.as_str().into()),
                setup_ms: number(link.setup_ms),
                rtt_us: number(link.stats.rtt_us),
                min_rtt_us: number(link.stats.min_rtt_us),
                congestion_window: number(link.stats.congestion_window),
                mtu: link.stats.mtu.into(),
                sent_packets: number(link.stats.sent_packets),
                lost_packets: number(link.stats.lost_packets),
                congestion_events: number(link.stats.congestion_events),
                sent_bytes: number(link.stats.sent_bytes),
                received_bytes: number(link.stats.received_bytes),
                lost_bytes: number(link.stats.lost_bytes),
                bandwidth_bps: link.stats.bandwidth_bps.map(number),
            }),
            lan: JsLanCounts {
                detected: number(value.lan.detected),
                missed: number(value.lan.missed),
            },
            direct_cooldown_ms: value.direct_cooldown_ms.map(number),
            cooldown_skips: number(value.cooldown_skips),
            lan_first: value.lan_first,
        }
    }
}

/// The direct attempt a tunnel is making right now.
#[napi(object)]
pub struct JsDirectAttempt {
    /// `negotiating`, `discovering`, `punching` or `committing`. Every stage
    /// before `committing` can still fall back to the relay.
    pub stage: String,
    pub elapsed_ms: f64,
    /// Attempts in flight, this one included.
    pub in_flight: u32,
}

/// Recovery counters from the running SDK, with no credential or payload data.
#[napi(object)]
pub struct JsPathDiagnostics {
    pub direct_sessions: f64,
    pub relay_sessions: f64,
    pub active_direct: f64,
    pub active_relay: f64,
    /// Direct sessions within one local network, open now and in all.
    pub active_lan: f64,
    pub lan_sessions: f64,
    /// Direct sessions over TCP, which only a local network carries, open
    /// now and in all.
    pub active_lan_tcp: f64,
    pub lan_tcp_sessions: f64,
    /// Setups that found the peer on a local network, and those of them that
    /// ended on another path or on the relay.
    pub lan_detected: f64,
    pub lan_missed: f64,
    pub fallbacks: f64,
    pub last_fallback: Option<String>,
    /// `lan`, `internet` or `private`: where the latest direct session this
    /// end set up runs.
    pub last_kind: Option<String>,
    /// The direct attempt in flight, if any.
    pub attempt: Option<JsDirectAttempt>,
    /// The newest direct session open now.
    pub link: Option<JsDirectLink>,
}

/// A direct session's path, as `diagnostics().direct.link` reports it.
#[napi(object)]
pub struct JsDirectLink {
    /// `lan`, `internet` or `private`.
    pub kind: String,
    /// `ipv4` or `ipv6`.
    pub family: String,
    /// How the peer address was learned: `host`, `public`, `mapped` or
    /// `peer_reflexive`.
    pub via: String,
    /// `quic`, or `tcp` within a local network. Over TCP the round trips are
    /// the handshake's, and the rest is zero.
    pub transport: String,
    pub peer: String,
    pub local_ip: Option<String>,
    /// The local interface the path leaves by, and its tier: `lan`,
    /// `physical`, `cellular`, `virtual` or `loopback`.
    pub interface: Option<String>,
    pub tier: Option<String>,
    /// `ipv6_prefix`, `public_ip_subnet` or `subnet`: what suggested a shared
    /// network before the setup.
    pub same_lan: Option<String>,
    pub timings: JsSetupTimings,
    pub candidates: JsCandidateCounts,
    /// QUIC's view of the path as of this read.
    pub rtt_us: f64,
    pub min_rtt_us: f64,
    pub congestion_window: f64,
    pub mtu: u32,
    pub sent_packets: f64,
    pub lost_packets: f64,
    pub congestion_events: f64,
    /// UDP bytes on the wire each way, and lost: QUIC's headers and probes
    /// included, so more than the application's.
    pub sent_bytes: f64,
    pub received_bytes: f64,
    pub lost_bytes: f64,
    /// The congestion controller's estimate of the path, in bits per second.
    pub bandwidth_bps: Option<f64>,
}

/// How long each stage of a direct setup took on this end; a stage it did
/// not run is absent.
#[napi(object)]
pub struct JsSetupTimings {
    pub negotiate_ms: Option<f64>,
    pub discover_ms: Option<f64>,
    pub stun_ms: Option<f64>,
    /// `ok`, `timeout`, `unavailable` or `cached`.
    pub stun: String,
    pub grant_ms: Option<f64>,
    pub first_pong_ms: Option<f64>,
    pub lan_first_pong_ms: Option<f64>,
    pub handshake_ms: Option<f64>,
    pub commit_ms: Option<f64>,
    pub setup_ms: f64,
}

/// The addresses a direct setup worked with.
#[napi(object)]
pub struct JsCandidateCounts {
    pub local: u32,
    pub local_lan: u32,
    /// Left out past the wire's limit of 8.
    pub truncated: u32,
    pub peer: u32,
    pub peer_lan: u32,
    pub probed: u32,
    pub answered: u32,
    pub handshakes: u32,
}

impl From<sdk::DirectLink> for JsDirectLink {
    fn from(link: sdk::DirectLink) -> Self {
        let timings = link.timings;
        let candidates = link.candidates;
        Self {
            kind: link.kind.as_str().into(),
            family: link.family.as_str().into(),
            via: link.via.as_str().into(),
            transport: link.transport.as_str().into(),
            peer: link.peer.to_string(),
            local_ip: link.local_ip.map(|ip| ip.to_string()),
            interface: link.interface,
            tier: link.tier.map(|tier| tier.as_str().into()),
            same_lan: link.same_lan.map(|evidence| evidence.as_str().into()),
            timings: JsSetupTimings {
                negotiate_ms: timings.negotiate_ms.map(number),
                discover_ms: timings.discover_ms.map(number),
                stun_ms: timings.stun_ms.map(number),
                stun: timings.stun.as_str().into(),
                grant_ms: timings.grant_ms.map(number),
                first_pong_ms: timings.first_pong_ms.map(number),
                lan_first_pong_ms: timings.lan_first_pong_ms.map(number),
                handshake_ms: timings.handshake_ms.map(number),
                commit_ms: timings.commit_ms.map(number),
                setup_ms: number(timings.setup_ms),
            },
            candidates: JsCandidateCounts {
                local: candidates.local,
                local_lan: candidates.local_lan,
                truncated: candidates.truncated,
                peer: candidates.peer,
                peer_lan: candidates.peer_lan,
                probed: candidates.probed,
                answered: candidates.answered,
                handshakes: candidates.handshakes,
            },
            rtt_us: number(link.stats.rtt_us),
            min_rtt_us: number(link.stats.min_rtt_us),
            congestion_window: number(link.stats.congestion_window),
            mtu: link.stats.mtu.into(),
            sent_packets: number(link.stats.sent_packets),
            lost_packets: number(link.stats.lost_packets),
            congestion_events: number(link.stats.congestion_events),
            sent_bytes: number(link.stats.sent_bytes),
            received_bytes: number(link.stats.received_bytes),
            lost_bytes: number(link.stats.lost_bytes),
            bandwidth_bps: link.stats.bandwidth_bps.map(number),
        }
    }
}

/// One event of a tunnel. In `diagnostics().events` consecutive repeats fold
/// into one entry, counted by `repeats`.
#[napi(object)]
#[derive(Default)]
pub struct JsTunnelEvent {
    /// Increases with every event, a fold included: remember the highest one
    /// handled to tell which entries are new.
    pub seq: f64,
    /// Time since the latest occurrence.
    pub age_ms: f64,
    pub repeats: u32,
    /// Outcomes, also kept in `diagnostics().events`: `relay_ready`,
    /// `relay_failed`, `network_changed`, `direct_established`,
    /// `direct_fallback` and `stopped`, the last event of a stream. Steps,
    /// from `subscribeEvents()` only: `phase_changed`, `direct_stage_changed`,
    /// `direct_abandoned`, `session_opened`, `session_closed`,
    /// `port_mapping_changed`, `lan_detected`, `path_selected`, `lan_missed`
    /// and `lan_tcp_missed`. A kind added by a later SDK reads `unknown`.
    pub kind: String,
    /// `relay_ready`: setup latency of the answering relay.
    pub latency_ms: Option<f64>,
    /// `relay_failed`: `dns`, `transport`, `timeout`, `service_unavailable`,
    /// `rejected` or `network_changed`.
    pub failure: Option<String>,
    /// `relay_failed`: delay before the next attempt.
    pub retry_in_ms: Option<f64>,
    /// `direct_established`: time from the first request to the open path.
    pub setup_ms: Option<f64>,
    /// `direct_fallback`: the stage the attempt left for the relay;
    /// `direct_stage_changed`: the stage it entered; `direct_abandoned`: the
    /// stage at which it was given up.
    pub stage: Option<String>,
    /// `direct_fallback`: why: `direct_not_offered`,
    /// `direct_negotiation_failed`, `direct_negotiation_timeout` or
    /// `direct_timeout_or_unreachable`. `lan_tcp_missed`: why TCP was not
    /// the path: `not_offered`, `no_shared_lan`, `refused`, `timed_out` or
    /// `handshake`.
    pub reason: Option<String>,
    /// `phase_changed`: the relay attempt's new phase.
    pub phase: Option<String>,
    /// `session_opened`, `session_closed`: the session's number in this
    /// tunnel, and `direct` or `relay`.
    pub session: Option<f64>,
    pub path: Option<String>,
    /// `session_closed`: how long it was open, and the application bytes it
    /// carried, sent from this end's local side and received.
    pub duration_ms: Option<f64>,
    pub sent_bytes: Option<f64>,
    pub received_bytes: Option<f64>,
    /// `port_mapping_changed`: the mapping as it now stands.
    pub mapping: Option<JsPortMappingStatus>,
    /// `lan_detected`, `lan_missed`: `ipv6_prefix`, `public_ip_subnet` or
    /// `subnet`.
    pub evidence: Option<String>,
    /// `path_selected`, and a direct `session_opened` or `session_closed`:
    /// `lan`, `internet` or `private`; with `path_selected`, `ipv4` or
    /// `ipv6`, and how the peer address was learned.
    pub link: Option<String>,
    pub family: Option<String>,
    pub via: Option<String>,
    /// `path_selected`: `quic`, or `tcp` within a local network.
    pub transport: Option<String>,
    /// `lan_missed`: where the session went instead, `internet` or
    /// `private`; `lan_tcp_missed`: `quic`. Absent when it took the relay.
    pub took: Option<String>,
    /// lan_detected: whether this end's LAN-first is on, so the peer's
    /// LAN addresses are probed first.
    pub preferred: Option<bool>,
}

impl From<sdk::TunnelEvent> for JsTunnelEvent {
    // `TunnelEventKind` is non-exhaustive, so the `_` arm has to stay; the
    // lint fails the build as soon as it would catch a kind this SDK has.
    #[deny(
        clippy::wildcard_enum_match_arm,
        reason = "every event kind of the SDK this is built with needs its JS name"
    )]
    fn from(event: sdk::TunnelEvent) -> Self {
        let mut js = Self {
            seq: number(event.seq),
            age_ms: number(event.age_ms),
            repeats: event.repeats,
            ..Self::default()
        };
        js.kind = match event.kind {
            sdk::TunnelEventKind::RelayReady { latency_ms } => {
                js.latency_ms = Some(number(latency_ms));
                "relay_ready"
            }
            sdk::TunnelEventKind::RelayFailed {
                failure,
                retry_in_ms,
            } => {
                js.failure = Some(failure.as_str().to_string());
                js.retry_in_ms = Some(number(retry_in_ms));
                "relay_failed"
            }
            sdk::TunnelEventKind::NetworkChanged => "network_changed",
            sdk::TunnelEventKind::DirectEstablished { setup_ms } => {
                js.setup_ms = Some(number(setup_ms));
                "direct_established"
            }
            sdk::TunnelEventKind::DirectFallback { stage, reason } => {
                js.stage = Some(stage.as_str().to_string());
                js.reason = Some(reason.as_str().into());
                "direct_fallback"
            }
            sdk::TunnelEventKind::Stopped => "stopped",
            sdk::TunnelEventKind::PhaseChanged { phase } => {
                js.phase = Some(phase.as_str().into());
                "phase_changed"
            }
            sdk::TunnelEventKind::DirectStageChanged { stage } => {
                js.stage = Some(stage.as_str().into());
                "direct_stage_changed"
            }
            sdk::TunnelEventKind::DirectAbandoned { stage } => {
                js.stage = Some(stage.as_str().into());
                "direct_abandoned"
            }
            sdk::TunnelEventKind::SessionOpened {
                session,
                path,
                link,
            } => {
                js.session = Some(number(session));
                js.path = Some(path.as_str().into());
                js.link = link.map(|link| link.as_str().into());
                "session_opened"
            }
            sdk::TunnelEventKind::SessionClosed {
                session,
                path,
                link,
                duration_ms,
                sent_bytes,
                received_bytes,
            } => {
                js.session = Some(number(session));
                js.path = Some(path.as_str().into());
                js.link = link.map(|link| link.as_str().into());
                js.duration_ms = Some(number(duration_ms));
                js.sent_bytes = Some(number(sent_bytes));
                js.received_bytes = Some(number(received_bytes));
                "session_closed"
            }
            sdk::TunnelEventKind::PortMappingChanged { mapping } => {
                js.mapping = Some((*mapping).into());
                "port_mapping_changed"
            }
            sdk::TunnelEventKind::LanDetected {
                evidence,
                preferred,
            } => {
                js.evidence = Some(evidence.as_str().into());
                js.preferred = Some(preferred);
                "lan_detected"
            }
            sdk::TunnelEventKind::PathSelected {
                link,
                family,
                via,
                transport,
            } => {
                js.link = Some(link.as_str().into());
                js.family = Some(family.as_str().into());
                js.via = Some(via.as_str().into());
                js.transport = Some(transport.as_str().into());
                "path_selected"
            }
            sdk::TunnelEventKind::LanMissed { evidence, took } => {
                js.evidence = Some(evidence.as_str().into());
                js.took = took.map(|link| link.as_str().into());
                "lan_missed"
            }
            sdk::TunnelEventKind::LanTcpMissed { reason, took } => {
                js.reason = Some(reason.as_str().into());
                js.took = took.map(|transport| transport.as_str().into());
                "lan_tcp_missed"
            }
            _ => "unknown",
        }
        .to_string();
        js
    }
}

#[napi(object)]
pub struct JsTunnelDiagnostics {
    pub relay_transport: String,
    /// Whether direct setups try a shared local network first: the
    /// tunnel's lanFirst, and never without direct.
    pub lan_first: bool,
    /// What direct setups ask a shared local network to carry, `quic` or
    /// `tcp`: a connection's lanTransport; `quic` without direct, and for a
    /// registration, which serves what each consumer asks for.
    pub lan_transport: String,
    pub direct: Option<JsPathDiagnostics>,
    /// Application bytes on each path, as `metrics().traffic`.
    pub traffic: JsPathTraffic,
    /// The smoothed round trip of control exchanges with the relay.
    pub relay_rtt_ms: Option<f64>,
    /// Present when the tunnel maps its direct endpoint's port.
    pub port_mapping: Option<JsPortMappingDiagnostics>,
    pub sdk_version: String,
    pub last_attempt_phase: String,
    pub attempts: f64,
    pub consecutive_failures: f64,
    pub last_failure: Option<String>,
    pub last_success_age_ms: Option<f64>,
    pub last_setup_latency_ms: Option<f64>,
    pub next_retry_in_ms: Option<f64>,
    pub dns_age_ms: Option<f64>,
    pub network_generation: f64,
    pub active_control_setups: u32,
    pub active_data_setups: u32,
    /// Recent history, oldest first; at most 16 entries.
    pub events: Vec<JsTunnelEvent>,
}

impl From<sdk::TunnelDiagnostics> for JsTunnelDiagnostics {
    fn from(value: sdk::TunnelDiagnostics) -> Self {
        Self {
            port_mapping: value.port_mapping.map(|mapping| JsPortMappingDiagnostics {
                current: mapping.current.into(),
                attempts: number(mapping.attempts),
                renewals: number(mapping.renewals),
                failures: number(mapping.failures),
            }),
            direct: value.direct.map(|d| JsPathDiagnostics {
                direct_sessions: number(d.direct_sessions),
                relay_sessions: number(d.relay_sessions),
                active_direct: number(d.active_direct),
                active_relay: number(d.active_relay),
                active_lan: number(d.active_lan),
                lan_sessions: number(d.lan_sessions),
                active_lan_tcp: number(d.active_lan_tcp),
                lan_tcp_sessions: number(d.lan_tcp_sessions),
                lan_detected: number(d.lan_detected),
                lan_missed: number(d.lan_missed),
                fallbacks: number(d.fallbacks),
                last_fallback: d.last_fallback,
                last_kind: d.last_kind.map(|kind| kind.as_str().into()),
                attempt: d.attempt.map(|attempt| JsDirectAttempt {
                    stage: attempt.stage.as_str().to_string(),
                    elapsed_ms: number(attempt.elapsed_ms),
                    in_flight: attempt.in_flight,
                }),
                link: d.link.map(JsDirectLink::from),
            }),
            traffic: value.traffic.into(),
            relay_rtt_ms: value.relay_rtt_ms.map(number),
            relay_transport: value.relay_transport.as_str().into(),
            lan_first: value.lan_first,
            lan_transport: value.lan_transport.as_str().into(),
            sdk_version: value.sdk_version.to_string(),
            last_attempt_phase: value.last_attempt_phase.as_str().to_string(),
            attempts: number(value.attempts),
            consecutive_failures: number(value.consecutive_failures),
            last_failure: value
                .last_failure
                .map(|failure| failure.as_str().to_string()),
            last_success_age_ms: value.last_success_age_ms.map(number),
            last_setup_latency_ms: value.last_setup_latency_ms.map(number),
            next_retry_in_ms: value.next_retry_in_ms.map(number),
            dns_age_ms: value.dns_age_ms.map(number),
            network_generation: number(value.network_generation),
            active_control_setups: value.active_control_setups as u32,
            active_data_setups: value.active_data_setups as u32,
            events: value.events.into_iter().map(JsTunnelEvent::from).collect(),
        }
    }
}
