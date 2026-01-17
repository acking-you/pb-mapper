# UDP Datagram Forwarding Notes

This document explains why UDP cannot be safely forwarded as a raw byte stream, what was changed in
pb-mapper to fix the issue, and how to validate the behavior.

## Background

UDP is message-oriented (datagram-based). Each read returns exactly one datagram (or a truncated
version of it). TCP is a byte stream: a read can return any number of bytes from the stream. That
semantic mismatch is the root cause of UDP failures when it is tunneled over a TCP stream without
explicit framing.

## Why stream-based UDP forwarding breaks

When UDP packets are forwarded over a TCP stream without framing:

- Datagram boundaries are lost: multiple UDP packets can be coalesced into one TCP read, or a single
  UDP packet can be split across multiple reads.
- Downstream UDP emitters will treat each stream chunk as one datagram, which corrupts protocols
  that expect exact packet boundaries (DNS, QUIC, RTP, game traffic, etc.).
- TCP adds retransmission and head-of-line blocking, which changes the delivery semantics of UDP and
  can stall real-time traffic.

### Visual examples (QUIC)

Below are ASCII diagrams showing how QUIC breaks when UDP boundaries are not preserved.

#### QUIC packet shape (very simplified)

You do not need prior QUIC knowledge to follow the examples. At a high level:

```
UDP Datagram
  [ UDP header ][ QUIC packet bytes ... ]
```

QUIC has two common header styles:

```
Short Header (1-RTT data, no explicit length field)
  [ ShortHdr ][ Packet Number ][ Encrypted Payload ... ]
  ^ length is inferred from the UDP datagram size

Long Header (Initial/Handshake, has a Length field)
  [ LongHdr ][ Version ][ DCID/SCID ][ Length ][ Packet Number ][ Encrypted Payload ... ]
```

So:

- Short header packets rely on the UDP datagram boundary for their length.
- Long header packets include a length, but still require the datagram to contain exactly that
  many bytes.

Example A: Two UDP datagrams are merged into one (coalesced)

```
Correct (UDP preserves boundaries)
Sender UDP datagrams:
  D1 (len=1200) -> [ QUIC 1-RTT Packet #1 | ShortHdr | Ciphertext(1200) ]
  D2 (len=800)  -> [ QUIC 1-RTT Packet #2 | ShortHdr | Ciphertext(800)  ]

Broken (TCP stream coalesces D1 + D2)
Tunnel turns it into:
  TCP stream bytes: [D1 bytes][D2 bytes]  ---> read() returns 2000 bytes once
Receiver sees one UDP datagram:
  D' (len=2000) -> [ QUIC 1-RTT Packet #1 ][ QUIC 1-RTT Packet #2 ]
                    ^ short header has no length field
                    ^ QUIC treats UDP datagram length as packet length
                    => AEAD decrypt uses wrong ciphertext length -> FAIL
                    => packet dropped -> no ACK -> loss timer -> retransmit -> still fails
```

Example B: One UDP datagram is split into two (fragmented by stream reads)

```
Correct
UDP D1 (len=1200) -> [ QUIC Initial Packet | LongHdr | Len=1200 | Ciphertext(1200) ]

Broken (TCP stream splits)
  read() #1 -> 600 bytes
  read() #2 -> 600 bytes
Receiver gets two UDP datagrams:
  D1a (len=600) -> [ QUIC Initial Packet | LongHdr | Len=1200 | ...half... ]
  D1b (len=600) -> [ ...tail bytes, not a valid packet start... ]
                    => D1a: length field says 1200 but only 600 available -> parse fail -> drop
                    => D1b: no valid QUIC header at start -> drop
                    => handshake stalls (Initial packets never accepted)
```

Example C: Visual pipeline (what QUIC expects vs what it receives)

```
Expected pipeline (UDP preserves packet boundary)
  UDP recv(1 datagram) --> QUIC parse --> AEAD decrypt --> ACK

Broken pipeline (UDP boundary lost)
  UDP recv(1 datagram that actually contains 2 packets)
    --> QUIC parse (short header, no length field)
      --> AEAD decrypt with wrong length -> FAIL
        --> drop -> no ACK -> retransmit -> still FAIL
```

## What was changed (high level)

1. The registration protocol now carries an explicit `is_datagram` flag so the server knows whether
   a service is UDP or TCP.
2. The pb-server uses datagram-aware forwarding when `is_datagram` is true.
3. Datagram forwarding uses message framing on the TCP hop to preserve packet boundaries.
4. UDP socket buffers are tuned and per-recv buffers are cleared to prevent stale data issues.
5. The encrypted writer now copies data before encryption to avoid mutating shared buffers.
6. Integration tests now use true UDP datagram echo semantics (not stream reads).

## Protocol change

`PbConnRequest::Register` now includes:

- `need_codec: bool`
- `is_datagram: bool`
- `key: String`

This is a breaking change for old clients that do not send `is_datagram`.

## Implementation details

### Datagram forwarding over TCP

On the TCP hop, datagrams are framed with a length header and forwarded as discrete messages. This
ensures that the receiver can reconstruct the original datagram boundaries exactly.

Key components:

- `DatagramReader` / `DatagramWriter`: treat each message as a single datagram.
- `NormalDatagramReader/Writer`: use the existing message header framing (length + body).
- `CodecDatagramReader/Writer`: same framing but with encryption/decryption.
- `start_datagram_forward`: bidirectional datagram transfer loop.

### UDP stream halves

`UdpStreamReadHalf` and `UdpStreamWriteHalf` expose explicit datagram APIs:

- `recv_datagram()` returns exactly one UDP packet.
- `send_datagram()` sends exactly one UDP packet and errors if partial send occurs.

This avoids using stream-style `read`/`write` for UDP.

### UDP socket tuning

- Buffer sizes are increased (recv/send) using `socket2` to reduce packet loss under load.
- `BytesMut` buffers are cleared before each `recv_buf_from` to prevent stale bytes.
- The max UDP payload is sized for IPv4 (`65,507` bytes).

### Encryption correctness

`CodecMessageWriter::write_msg` now copies the input buffer before encryption to avoid in-place
mutation of shared buffers. This prevents cross-talk between tasks that reuse the same slice.

## Behavior difference vs old approach

Old behavior (broken for UDP):

- UDP traffic was forwarded using stream copy semantics on the TCP hop.
- Datagram boundaries were lost, causing protocol corruption.

New behavior (correct):

- UDP traffic is forwarded as framed messages on the TCP hop.
- Each UDP datagram is preserved 1:1 in both directions.

## Tests

Integration tests now perform real UDP datagram echo with sequence validation and a warm-up phase.
This reduces false positives from pipeline startup timing.

Recommended commands:

- Codec path:
  - `CARGO_HOME=target/cargo-home CARGO_TARGET_DIR=target cargo test -p pb-mapper --test test_delay test_pb_mapper_server_codec -- --nocapture`
- No-codec path (ignored by default):
  - `CARGO_HOME=target/cargo-home CARGO_TARGET_DIR=target cargo test -p pb-mapper --test test_delay test_pb_mapper_server_no_codec -- --ignored --nocapture`

## Migration notes

- Older clients that do not send `is_datagram` will fail to register.
- Update both server and client binaries together.

## TODO: UDP data-plane tunnel (control plane stays TCP)

Goal: keep the reliable control channel on TCP, but move the UDP payload path to a pure UDP tunnel
to reduce latency and head-of-line blocking.

Minimal design notes:

- Control plane (TCP):
  - Register `key` and negotiate session parameters.
  - Server returns `udp_port` + `session_token`.
- Data plane (UDP):
  - Payloads flow over `udp_port` with a minimal header (e.g., `token` or `session_id`).
  - Server validates token and forwards to the peer's last-seen UDP address.
- Required behaviors even if the game handles loss:
  - NAT keepalive (periodic empty datagrams).
  - Address update on NAT rebinding.
  - Basic injection protection (token validation).
  - Conservative MTU target (~1200 bytes) to avoid fragmentation.

This is intentionally minimal; reliability/retransmission is left to the game protocol itself.
