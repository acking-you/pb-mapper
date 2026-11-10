# pb-mapper relay

![Docker Pulls](https://img.shields.io/docker/pulls/ackingliu/pb-mapper)
![Docker Image Size](https://img.shields.io/docker/image-size/ackingliu/pb-mapper)

The [pb-mapper](https://github.com/acking-you/pb-mapper) relay as a container.
One port carries everything: over TCP it relays every service, and over UDP, on
the same number, it lets the two ends of a tunnel set up a direct path, so
their traffic crosses the relay only when no direct path works.

## Run

```bash
docker run -d --name pb-mapper \
  --restart unless-stopped \
  -p 7666:7666/tcp -p 7666:7666/udp \
  -e RUST_LOG=error \
  -v pb-mapper-auth:/var/lib/pb-mapper/auth \
  ackingliu/pb-mapper:latest
```

With Compose, save this as `compose.yaml` and run `docker compose up -d`:

```yaml
services:
  pb-mapper:
    container_name: pb-mapper
    image: ackingliu/pb-mapper:latest
    environment:
      RUST_LOG: error
    volumes:
      - pb-mapper-auth:/var/lib/pb-mapper/auth
    ports:
      - "7666:7666/tcp"
      - "7666:7666/udp"   # direct (peer-to-peer) coordination
    restart: unless-stopped

volumes:
  pb-mapper-auth:
```

The first start creates a random administrator key in the volume. Read it on
the Docker host:

```bash
docker exec pb-mapper cat /var/lib/pb-mapper/auth/admin.key
```

Keep the volume: losing it changes the administrator key and loses every
temporary credential. Open 7666/tcp and 7666/udp in the host's firewall and in
the cloud security group.

The other roles come from the same `pb-mapper` binary, on the computers at the
two ends: `pb-mapper register` next to a service, `pb-mapper connect` where you
need it. See the
[quick start](https://github.com/acking-you/pb-mapper#get-started).

## Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `PB_MAPPER_PORT` | `7666` | The port the relay listens on, over TCP and UDP |
| `USE_IPV6` | `false` | `true` listens on IPv6 (`::`) instead of IPv4 |
| `MSG_HEADER_KEY` | unset | A 32-character administrator key, used only to initialize a new auth volume |
| `USE_MACHINE_MSG_HEADER_KEY` | `false` | Legacy: derive the administrator key from the hostname and MAC address, while no `admin.key` exists |
| `PB_MAPPER_AUTH_STATE_DIR` | `/var/lib/pb-mapper/auth` | Where the encrypted authentication state is kept |
| `PB_MAPPER_RELAY_ONLY` | `false` | `true` serves the relay alone: no UDP port, no direct connections |
| `PB_MAPPER_DIRECT_ADVERTISE` | unset | The public `IP:PORT` for direct coordination when the UDP port is mapped differently from the TCP one; by default clients use the address they dialed |
| `RUST_LOG` | `info` | Log level: `error`, `warn`, `info`, `debug` or `trace` |

For another port, set `PB_MAPPER_PORT` and publish that port over both TCP and
UDP. With `USE_IPV6: true`, publish on the IPv6 address:
`"[::]:7666:7666/tcp"` and `"[::]:7666:7666/udp"`.

## Tags

| Tag | Image |
| --- | --- |
| `latest` | The newest release |
| `X.Y.Z` | One release, also as `vX.Y.Z`; `X.Y` and `X` follow the newest release of that line |

Each tag is one image for linux/amd64 and linux/arm64. Either half is also
tagged alone, as `<tag>-x86_64_musl` and `<tag>-aarch64_musl`, the only form
releases before 0.11.0 have. For any other architecture, build the binary and
then `docker/pb-mapper.dockerfile`, as
`scripts/release/release-docker-image.sh` in the
[repository](https://github.com/acking-you/pb-mapper) does.

## Security

- Keep `admin.key` on the relay, and give each workload an expiring `pbmt1_`
  credential issued with `pb-mapper admin`
  ([credentials](https://github.com/acking-you/pb-mapper/blob/master/docs/user-guide.md#administrator-and-temporary-credentials)).
- Publish 7666/tcp and 7666/udp only to the networks that need them.
- Use `register --codec` when the application's own protocol is plaintext.
- Update by pulling the tag again: `docker compose pull && docker compose up -d`.

## Links

[User guide](https://github.com/acking-you/pb-mapper/blob/master/docs/user-guide.md) ·
[README](https://github.com/acking-you/pb-mapper/blob/master/README.md) ·
[Releases](https://github.com/acking-you/pb-mapper/releases) ·
[Issues](https://github.com/acking-you/pb-mapper/issues)
