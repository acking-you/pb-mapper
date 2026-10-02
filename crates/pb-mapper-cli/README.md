# pb-mapper CLI

`pb-mapper-cli` provides the unified `pb-mapper` executable for running a
relay, registering private TCP/UDP services, connecting to them, inspecting
status, and administering credentials.

## Install

```bash
cargo install pb-mapper-cli --locked
```

The package name is `pb-mapper-cli`; the installed executable is `pb-mapper`.
The same executable contains both client and server roles:

```bash
pb-mapper server --port 7666
pb-mapper register tcp --server relay.example.com:7666 --key app --addr 127.0.0.1:8080
pb-mapper connect tcp --server relay.example.com:7666 --key app --addr 127.0.0.1:9090
```

Set `MSG_HEADER_KEY` to the administrator or temporary credential used by the
relay. See the [user guide](https://github.com/acking-you/pb-mapper/blob/master/docs/user-guide.md)
for credential setup, deployment, and all commands.
