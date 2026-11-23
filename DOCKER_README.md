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
  --sysctl net.ipv4.tcp_slow_start_after_idle=0 \
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
    sysctls:
      net.ipv4.tcp_slow_start_after_idle: 0   # see TCP tuning below
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
| `PB_MAPPER_TCP_CONGESTION` | unset | The TCP congestion control the relay sends with, such as `bbr` (see TCP tuning below); unset keeps the kernel's default |
| `RUST_LOG` | `info` | Log level: `error`, `warn`, `info`, `debug` or `trace` |

For another port, set `PB_MAPPER_PORT` and publish that port over both TCP and
UDP. With `USE_IPV6: true`, publish on the IPv6 address:
`"[::]:7666:7666/tcp"` and `"[::]:7666:7666/udp"`.

## TCP tuning

Two kernel settings decide how quickly the relay sends after a pause and over
a lossy path. Both change only what the relay sends, such as a remote
desktop's picture on its way to the viewer; what the shared computer sends to
the relay runs on that computer's own network stack.

- `net.ipv4.tcp_slow_start_after_idle`: Linux restarts the congestion window of
  a TCP connection that was idle, so the first picture after a still screen
  starts slowly again. The examples above turn this off with `--sysctl` and
  `sysctls`. It applies to the container's own network namespace only, needs
  Linux 4.15 or later (on an older kernel the container does not start: remove
  the setting there) and cannot be set with `--network host`. The relay logs
  `relay_tcp_idle_restart` at `info` while it is on.
- `PB_MAPPER_TCP_CONGESTION=bbr`: every connection the relay accepts sends with
  BBR, which random loss on a long cross-border or mobile path does not slow
  as it does Linux's default, Cubic. It is off by default because BBR shares a
  congested link with Cubic flows less evenly. The Docker host needs the
  `tcp_bbr` module (`sudo modprobe tcp_bbr`, and `tcp_bbr` in a file under
  `/etc/modules-load.d/` to keep it after a reboot), and either `bbr` in the
  host's `net.ipv4.tcp_allowed_congestion_control` or the container started
  with `--cap-add NET_ADMIN` (`cap_add: [NET_ADMIN]` in Compose). When the
  kernel refuses, the relay logs `relay_tcp_congestion_unavailable` with the
  fix and keeps the default.

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
