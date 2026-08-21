//! Payload generators and the load drivers the cases assert with.

use std::net::SocketAddr;
use std::time::Duration;

use pb_mapper_protocol::{MessageReader, MessageWriter, NormalMessageReader, NormalMessageWriter};
use rand::RngExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{Instant, timeout};
use uni_stream::addr::ToSocketAddrs;
use uni_stream::stream::{StreamProvider, StreamSplit};
use uni_stream::udp::tune_udp_socket;

use crate::{PROBE, READY_TIMEOUT, UDP_TEST_PAYLOAD_MAX};

/// What an echo server tagged with `tag` replies to `payload`.
///
/// The tag is a leading byte identifying which echo server answered; see
/// [`crate::TunnelSpec::echo_tag`]. Untagged servers echo the payload verbatim.
pub fn tagged_echo(tag: Option<u8>, payload: &[u8]) -> Vec<u8> {
    let mut expected = Vec::with_capacity(payload.len() + 1);
    expected.extend(tag);
    expected.extend_from_slice(payload);
    expected
}

/// Accumulates the time spent inside its scope and prints that slice.
pub struct TimerTickGuard<'a> {
    ins: Instant,
    mut_duration: &'a mut Duration,
}

impl<'a> TimerTickGuard<'a> {
    pub fn new(mut_duration: &'a mut Duration) -> Self {
        Self {
            ins: Instant::now(),
            mut_duration,
        }
    }
}

impl Drop for TimerTickGuard<'_> {
    fn drop(&mut self) {
        let duration = Instant::now() - self.ins;
        *self.mut_duration += duration;
        println!("duration:{duration:?}");
    }
}

pub async fn connected_udp_socket(addr: SocketAddr) -> UdpSocket {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    tune_udp_socket(&socket);
    socket.connect(addr).await.unwrap();
    socket
}

/// Round-trip one probe datagram on `socket` and check it against `expected`.
///
/// The tunnel keys UDP streams by source address, so every new socket starts a
/// new stream and its first datagram can be dropped while the relay sets that
/// stream up. Callers that go on to assert payload equality warm their own socket
/// with this first.
pub async fn probe_udp_socket(socket: &UdpSocket, expected: &[u8]) -> Result<(), String> {
    socket
        .send(PROBE)
        .await
        .map_err(|error| format!("udp probe send failed: {error}"))?;
    let mut buf = vec![0u8; 65_507];
    let len = timeout(Duration::from_millis(500), socket.recv(&mut buf))
        .await
        .map_err(|_| "udp probe timed out".to_string())?
        .map_err(|error| format!("udp probe recv failed: {error}"))?;
    if &buf[..len] == expected {
        Ok(())
    } else {
        Err(format!("udp probe echoed {len} bytes: {:?}", &buf[..len]))
    }
}

pub async fn wait_until_udp_socket_forwards(socket: &UdpSocket, tag: Option<u8>) {
    let expected = tagged_echo(tag, PROBE);
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut last_error = String::from("no attempt completed");
    while Instant::now() < deadline {
        match probe_udp_socket(socket, &expected).await {
            Ok(()) => return,
            Err(error) => last_error = error,
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("udp socket never forwarded through the tunnel: {last_error}");
}

/// Random payload; the length is random too, so framing is exercised at many sizes.
pub fn gen_random_msg(max_len: usize) -> Vec<u8> {
    let len = rand::rng().random_range(0_usize..max_len);
    let mut vec = Vec::with_capacity(len);
    for _ in 0..len {
        vec.push(rand::rng().random_range(0..212));
    }
    vec
}

/// Send `rounds` × `burst` datagrams through the tunnel and assert each comes back.
pub async fn run_udp_datagram_echo(addr: SocketAddr, rounds: usize, burst: usize, tag: Option<u8>) {
    let socket = connected_udp_socket(addr).await;
    wait_until_udp_socket_forwards(&socket, tag).await;
    let mut buf = vec![0u8; 65_507];

    for round in 0..rounds {
        for seq in 0..burst {
            let mut msg = (seq as u32).to_be_bytes().to_vec();
            if round == 0 && seq == 0 {
                msg.extend(vec![0u8; UDP_TEST_PAYLOAD_MAX]);
            } else {
                msg.extend(gen_random_msg(UDP_TEST_PAYLOAD_MAX));
            }
            socket.send(&msg).await.unwrap();
            let expected = tagged_echo(tag, &msg);
            let offset = usize::from(tag.is_some());
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let wait = deadline.saturating_duration_since(Instant::now());
                let len = match timeout(wait, socket.recv(&mut buf)).await {
                    Ok(Ok(len)) => len,
                    Ok(Err(err)) => panic!("udp recv error: {err}"),
                    Err(_) => panic!("udp echo timeout; missing seq: {seq}"),
                };
                if len < offset + 4 {
                    continue;
                }
                // A datagram from an earlier sequence number may still be in
                // flight; skip it rather than failing the comparison.
                let echoed_seq = u32::from_be_bytes(buf[offset..offset + 4].try_into().unwrap());
                if echoed_seq != seq as u32 {
                    continue;
                }
                assert_eq!(expected, &buf[..len]);
                break;
            }
        }
    }
}

/// Round-trip `times` × 10 random payloads over one stream and assert each echo.
///
/// This frames its payloads, so the echo server must be byte-transparent — an
/// echo tag would shift every frame header by a byte. Use [`run_raw_tcp_echo`]
/// for a tagged tunnel.
pub async fn run_echo_delay<P: StreamProvider, A: ToSocketAddrs + Send>(addr: A, times: usize) {
    let mut stream = P::from_addr(addr).await.unwrap();
    let (mut reader, mut writer) = stream.split();
    let mut reader = NormalMessageReader::new(&mut reader);
    let mut writer = NormalMessageWriter::new(&mut writer);
    let mut duration = Duration::default();
    for _ in 0..times {
        let expected = gen_random_msg(2000);
        for _ in 0..10 {
            let msg = {
                let _guard = TimerTickGuard::new(&mut duration);
                writer.write_msg(&expected).await.unwrap();
                reader.read_msg().await.unwrap()
            };

            assert_eq!(expected, msg);
        }
    }
    println!("{times} rounds of 10 random data echo delay tests each took a total of {duration:?}");
}

/// Round-trip `rounds` random payloads over one raw TCP stream, tag included.
///
/// Unframed, so an echo tag stays a tag instead of shifting a frame header. The
/// tunnel's local side is byte-transparent either way, so the only thing framing
/// buys a test is the length delimiter — which this replaces by reading exactly
/// as many bytes as it expects.
pub async fn run_raw_tcp_echo(addr: SocketAddr, rounds: usize, tag: Option<u8>) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    for _ in 0..rounds {
        // At least one byte, so a payload is never indistinguishable from a bare tag.
        let payload = gen_random_msg(2000);
        let payload = if payload.is_empty() {
            vec![7u8]
        } else {
            payload
        };
        let expected = tagged_echo(tag, &payload);
        stream.write_all(&payload).await.unwrap();
        let mut echoed = vec![0u8; expected.len()];
        timeout(Duration::from_secs(5), stream.read_exact(&mut echoed))
            .await
            .expect("raw tcp echo timed out")
            .expect("raw tcp echo failed");
        assert_eq!(expected, echoed);
    }
}

/// One raw payload through the tunnel, compared against `expected`.
pub async fn raw_tcp_probe(
    addr: SocketAddr,
    payload: &[u8],
    expected: &[u8],
) -> Result<(), String> {
    let mut stream = timeout(Duration::from_secs(1), TcpStream::connect(addr))
        .await
        .map_err(|_| "tcp connect timed out".to_string())?
        .map_err(|error| format!("tcp connect failed: {error}"))?;
    stream
        .write_all(payload)
        .await
        .map_err(|error| format!("tcp probe write failed: {error}"))?;
    let mut echoed = vec![0u8; expected.len()];
    timeout(Duration::from_secs(1), stream.read_exact(&mut echoed))
        .await
        .map_err(|_| "tcp probe read timed out".to_string())?
        .map_err(|error| format!("tcp probe read failed: {error}"))?;
    if echoed == expected {
        Ok(())
    } else {
        Err(format!("tcp probe echoed {echoed:?}"))
    }
}
