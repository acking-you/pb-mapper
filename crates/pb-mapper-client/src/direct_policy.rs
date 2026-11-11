//! How a tunnel's direct setups go, as its options say: decided by the SDK and
//! carried through the tunnel workers to the direct core.

use crate::sdk::{LanTransport, PortMapping};

/// How a tunnel's direct setups go. A worker given none keeps to the relay.
/// It exists without `p2p` as well, since the workers that carry it do,
/// though only the direct core reads it.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(feature = "p2p"),
    expect(
        dead_code,
        reason = "only the direct core, behind p2p, reads its fields"
    )
)]
pub(crate) struct DirectPolicy {
    pub port_mapping: PortMapping,
    pub lan_first: bool,
    /// What a consumer asks a shared LAN to carry.
    pub lan_transport: LanTransport,
    /// Whether a provider serves TCP on a shared LAN to a consumer that asks.
    pub lan_tcp: bool,
}
