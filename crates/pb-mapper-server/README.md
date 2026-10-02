# pb-mapper server

`pb-mapper-server` is the relay-server library used by pb-mapper. It accepts
authenticated service registrations and client subscriptions, then routes TCP
or UDP traffic between them.

Applications embedding the relay can depend on the library directly:

```toml
[dependencies]
pb-mapper-server = "0.5"
```

To install and run the ready-made server program, install the unified CLI:

```bash
cargo install pb-mapper-cli --locked
pb-mapper server --port 7666
```

See the [user guide](https://github.com/acking-you/pb-mapper/blob/master/docs/user-guide.md)
for authentication and deployment instructions.
