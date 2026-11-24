# pb-mapper server

`pb-mapper-server` is the relay-server library used by pb-mapper. It accepts
authenticated service registrations and client subscriptions, then routes TCP
or UDP traffic between them. With its UDP service enabled
(`run_server_on_listener_with_direct`, `p2p` feature), it also coordinates
direct peer-to-peer sessions between endpoints and stays their relay
fallback. `DirectServerConfig::advertise` may leave the IP unspecified, in
which case clients pair the announced port with the relay address they dialed.

Optional QUIC relay uses the independent `relay-quic` feature and
`run_server_on_listener_with_options` with
`RelayServerOptions { direct, quic: Some(QuicServerConfig { listen, advertise_port: 0 }) }`.
Use a separate UDP port from direct coordination. TCP remains available for old
clients and authenticated certificate discovery; existing entry points keep
their TCP behavior.

On Linux, `set_tcp_congestion(&listener, &TcpCongestion::new("bbr")?)` before
the listener goes to `run_server_on_listener_with_options` has every
connection it accepts send with that congestion control, as
`pb-mapper server --tcp-congestion bbr` does; a kernel that refuses it returns
`TcpCongestionError`, whose `fix()` says how to allow it.

Applications embedding the relay can depend on the library directly:

```toml
[dependencies]
pb-mapper-server = "0.8"
```

To install and run the ready-made server program, install the unified CLI:

```bash
cargo install pb-mapper-cli --locked
pb-mapper server --port 7666   # 7666/tcp relay, 7666/udp direct coordination
```

See the [user guide](https://github.com/acking-you/pb-mapper/blob/master/docs/user-guide.md)
for authentication and deployment instructions.
