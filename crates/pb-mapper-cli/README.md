# pb-mapper CLI

`pb-mapper-cli` provides the unified `pb-mapper` executable: run a relay,
publish private TCP/UDP services, reach them on a local port, inspect status,
and administer credentials. Connections go peer to peer over QUIC when the
networks allow it and fall back to the relay when they do not; nothing needs to
be configured for either.

## Install

```bash
cargo install pb-mapper-cli --locked
```

The package name is `pb-mapper-cli`; the installed executable is `pb-mapper`.
The same executable contains every role:

```bash
# on a public host: relay on 7666/tcp, hole punching on 7666/udp
pb-mapper server

# next to the private service, and wherever you need it
pb-mapper register tcp --server relay.example.com:7666 --key app --addr 127.0.0.1:8080
pb-mapper connect  tcp --server relay.example.com:7666 --key app --addr 127.0.0.1:9090

# which path each connection took
pb-mapper status sessions --server relay.example.com:7666
```

Open both `7666/tcp` and `7666/udp` on the relay. Add `--relay-only` to
`server`, `register` or `connect` to keep that side on the relay.

The relay carrier defaults to TCP. For optional QUIC relay, start `server`
with `--quic-listen 0.0.0.0:7667`, open UDP 7667, and add
`--relay-transport quic` to `register` and/or `connect`. Keep TCP 7666 for
authenticated discovery. `--relay-only` disables peer-to-peer direct sessions
while still permitting the explicit QUIC relay listener.

Set `MSG_HEADER_KEY` to the administrator key or to a temporary `pbmt1_`
credential issued with `pb-mapper admin key issue`. See the
[user guide](https://github.com/acking-you/pb-mapper/blob/master/docs/user-guide.md)
for credential setup, deployment, and every command.
