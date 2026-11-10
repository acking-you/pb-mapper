//! Shared end-to-end scaffolding for the integration tests.
//!
//! The unit of reuse is a [`Relay`] — one authenticated `pb-mapper server` on a
//! reserved loopback port with its own authentication state directory — and a
//! [`Tunnel`] on top of it: an echo server, a `register`, and a `connect`. Both
//! are ordinary values with a `Drop` that tears their tasks down, so a test file
//! stands up a complete flow in one line and needs no shared fixture, no test
//! ordering, and no cleanup code.
//!
//! ```ignore
//! let harness = TunnelHarness::start(Transport::Tcp, false).await;
//! run_framed_tcp_echo(harness.tunnel_addr(), 10).await;
//! ```
//!
//! For anything the one-liner cannot express — a temporary credential, two
//! tunnels sharing a relay, revoking a key under a live tunnel — drive the two
//! halves separately:
//!
//! ```ignore
//! let relay = Relay::start("temp-key").await;
//! let (key_id, credential) = relay.issue_credential(Duration::from_secs(60), "tcp").await;
//! let tunnel = relay
//!     .start_tunnel(TunnelSpec::new(Transport::Tcp).credential(credential))
//!     .await;
//! ```
//!
//! A case that needs a slower network puts a [`shaped_link`] hop, with its
//! delay and rate, in front of a [`Relay`] and connects through the hop.
//!
//! A case about the SDK rather than the CLI entry points builds its own
//! backend with [`tcp_echo`] or [`udp_echo`], and its tunnel with
//! [`admin_client`], [`publish`] and [`subscribe`].
//!
//! Everything here is a readiness probe rather than a sleep: [`Relay`] polls the
//! `Keys` status to see a registration, and [`Tunnel::start`] returns only once a
//! payload has made the full round trip. That is why the cases carry no timing
//! assumptions and can all run concurrently.
//!
//! This is a real crate rather than a `tests/common/mod.rs` module because that
//! module is compiled separately into every test binary, and whatever a given
//! binary happens not to use is reported as dead code — fatal under
//! `-D warnings`.

// The entire crate is test support, so the workspace's `unwrap`/`expect` denial
// is lifted here the same way it is inside a `tests/` target. `clippy.toml`'s
// test exemptions do not reach a library crate, which is not `#[cfg(test)]`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pb_mapper_auth::{AuthConfig, LegacyProtocolPolicy};
use pb_mapper_core::checksum::{ADMIN_KEY_LEN, Credential, set_process_msg_header_key};
use pb_mapper_core::config::init_tracing;

mod control;
mod echo;
mod poll;
mod relay;
mod sdk;
mod shaped;
mod traffic;
mod tunnel;

pub use control::{V2ControlSpec, register_v2_control};
pub use echo::{Echo, tcp_echo, udp_echo};
pub use relay::{Relay, loopback_direct, loopback_quic};
pub use sdk::{admin_client, publish, subscribe};
pub use shaped::{Shape, shaped_link};
pub use traffic::{
    connected_udp_socket, gen_random_msg, probe_udp_socket, raw_tcp_probe, run_framed_tcp_echo,
    run_raw_tcp_echo, run_udp_datagram_echo, tagged_echo, wait_until_udp_socket_forwards,
};
pub use tunnel::{Tunnel, TunnelHarness, TunnelSpec};

/// The administrator key every test relay starts with.
///
/// A fixed key rather than a random one: it is also the process credential (see
/// [`init_test_env`]), and one value keeps that a single write per process.
pub const TEST_ADMIN_KEY: &str = "0123456789abcdefghijklmnopqrstuv";

/// How long any readiness probe polls before failing the test.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// The payload every readiness probe round-trips.
pub const PROBE: &[u8] = b"pb-mapper-probe";

/// Upper bound on generated UDP payloads, comfortably inside one datagram.
pub const UDP_TEST_PAYLOAD_MAX: usize = 1200;

/// Upper bound on generated raw-TCP payloads.
///
/// Deliberately past both one segment and the 8 KiB initial forwarding buffer, so
/// a driver that assumed a write arrives as one read is caught here rather than by
/// whoever later raises a payload size.
pub const RAW_TCP_PAYLOAD_MAX: usize = 20_000;

/// Tracing plus the process credential, set up once per test process.
///
/// The framing helper in this crate, [`run_framed_tcp_echo`], writes its own
/// checksummed frames, and `checksum_for` fails closed without a process
/// credential — so one has to exist before any case runs. It is
/// unrelated to which credential a tunnel authenticates with: `pb-mapper`'s local
/// side is byte-transparent, so these frames are only ever read back by the same
/// process that wrote them.
///
/// This establishes the baseline for the whole test binary: the `LazyLock` runs
/// once, before any case can observe the credential, and never writes again.
/// A case that needs a different credential for a tunnel passes it to
/// [`TunnelSpec::credential`], which is per tunnel and touches nothing global.
/// A case that sets or clears the process credential itself holds
/// [`PROCESS_CREDENTIAL_TEST_LOCK`](pb_mapper_core::test_support::PROCESS_CREDENTIAL_TEST_LOCK),
/// and so does every case in its binary that relies on the credential: the
/// framing in every concurrent case reads it.
pub fn init_test_env() {
    static TEST_ENV: LazyLock<()> = LazyLock::new(|| {
        init_tracing();
        set_process_msg_header_key(Some(TEST_ADMIN_KEY)).unwrap();
    });
    *TEST_ENV
}

/// [`TEST_ADMIN_KEY`]'s bytes. A key of any other length fails to compile.
const ADMIN_KEY: [u8; ADMIN_KEY_LEN] = match TEST_ADMIN_KEY.as_bytes().first_chunk() {
    Some(key) if TEST_ADMIN_KEY.len() == ADMIN_KEY_LEN => *key,
    _ => panic!("TEST_ADMIN_KEY must be exactly one administrator key"),
};

/// The administrator credential matching [`TEST_ADMIN_KEY`].
pub fn admin_credential() -> Credential {
    Credential::Admin(ADMIN_KEY)
}

/// The 32 raw bytes of [`TEST_ADMIN_KEY`], as [`pb_mapper_auth::AuthRuntime`] wants them.
pub fn admin_key_bytes() -> [u8; 32] {
    ADMIN_KEY
}

/// A private authentication state directory, unique per relay.
///
/// Each relay takes an exclusive `flock` on `auth.lock` inside its state
/// directory, so two relays must never share one.
pub fn auth_config(label: &str) -> AuthConfig {
    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    AuthConfig {
        state_dir: std::env::temp_dir().join(format!(
            "pb-mapper-testkit-{}-{label}-{sequence}",
            std::process::id()
        )),
        max_temporary_keys: 64,
        max_temporary_key_ttl: Duration::from_secs(3600),
        legacy_protocol: LegacyProtocolPolicy::Allow,
    }
}

/// Which transport a tunnel carries, end to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Udp,
}

impl Transport {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }

    pub fn is_datagram(self) -> bool {
        self == Self::Udp
    }
}

/// Where [`reserve_addr`] allocates from: below every platform's ephemeral range
/// (Linux defaults to 32768, macOS and Windows to 49152), so a port handed out
/// here is never one the kernel also assigns to a `:0` bind elsewhere.
const RESERVED_PORT_FLOOR: u16 = 20_000;
const RESERVED_PORT_CEILING: u16 = 32_767;

/// How many ports one test process owns before wrapping into another's slots.
const RESERVED_PORTS_PER_PROCESS: u16 = 64;

/// Pick a free loopback port for an address the caller cannot bind itself.
///
/// TCP and UDP have separate port spaces, so this has to use the same protocol the
/// caller will bind. A relay and an echo server keep the socket they bound; this is
/// for `connect`, whose bind happens inside the client — which binds the address
/// again after a local listener failure — so it cannot be handed a pre-bound
/// socket. A test that can own its socket outright should do that instead of
/// calling this.
///
/// The socket still has to be dropped before the client binds, so the port is not
/// held for the caller — what this guarantees is that nothing else is going to pick
/// the same one:
///
/// - It allocates outside the ephemeral range, so no `:0` bind anywhere in the
///   process, the test binary, or the machine can be handed this port.
/// - Candidates advance from a per-process base, so two concurrent cases in one
///   binary never see the same port, and two test binaries running at once start
///   from different bases.
/// - Every candidate is proven free by binding it, so wrapping into a slot another
///   process is using costs a retry rather than a flake.
pub async fn reserve_addr(transport: Transport) -> std::net::SocketAddr {
    static NEXT_PORT: AtomicUsize = AtomicUsize::new(0);
    let span = u32::from(RESERVED_PORT_CEILING - RESERVED_PORT_FLOOR + 1);
    let slots = span / u32::from(RESERVED_PORTS_PER_PROCESS);
    let base = u32::from(RESERVED_PORT_FLOOR)
        + (std::process::id() % slots) * u32::from(RESERVED_PORTS_PER_PROCESS);

    for _ in 0..span {
        let offset = NEXT_PORT.fetch_add(1, Ordering::Relaxed) as u32;
        let port =
            RESERVED_PORT_FLOOR + ((base - u32::from(RESERVED_PORT_FLOOR) + offset) % span) as u16;
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let bound = match transport {
            Transport::Tcp => tokio::net::TcpListener::bind(addr)
                .await
                .map(|listener| listener.local_addr()),
            Transport::Udp => tokio::net::UdpSocket::bind(addr)
                .await
                .map(|socket| socket.local_addr()),
        };
        if let Ok(Ok(addr)) = bound {
            return addr;
        }
    }
    panic!("no free loopback port in {RESERVED_PORT_FLOOR}..={RESERVED_PORT_CEILING}");
}
