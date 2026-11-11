//! The UDP session timeout, which uni-stream keeps for the whole process.
//!
//! uni-stream 0.3.5 keeps it in a `static mut` that it reads once, unguarded,
//! when the process makes its first UDP stream, and keeps for every UDP
//! stream after. A connect tunnel writes 120 s there as it starts; a
//! publisher's UDP sessions take uni-stream's 20 s unless a connect tunnel
//! started first. Written on one thread while another made the first UDP
//! stream, it could be read half written: as no time at all, among others.
//!
//! So the write, and every UDP stream this crate makes, go through one
//! [`Once`]: a connect tunnel as it starts, a publisher before it dials its
//! UDP service. Whichever comes first decides, as it did, so each process
//! keeps the timeout it had; the write and the read are just never at once.
//! A connect tunnel's own UDP streams come from a listener it binds after.

use std::sync::Once;
use std::time::Duration;

/// A connect tunnel's UDP sessions end after this long without a datagram.
const CONNECT_UDP_TIMEOUT: Duration = Duration::from_secs(120);

static DECIDED: Once = Once::new();

/// A connect tunnel is starting: UDP streams made from now on time out after
/// [`CONNECT_UDP_TIMEOUT`], unless a publisher of this process made one
/// first.
pub(crate) fn connect_tunnel_starting() {
    DECIDED.call_once(|| uni_stream::udp::set_custom_timeout(CONNECT_UDP_TIMEOUT));
}

/// A publisher is about to make a UDP stream to its service: from here on
/// nothing writes the timeout, which this stream may be the first to read.
pub(crate) fn publisher_udp_stream() {
    DECIDED.call_once(|| {});
}
