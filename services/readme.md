# systemd units

Install the unified binary and the unit you need:

```bash
sudo install -m 0755 target/release/pb-mapper /usr/local/bin/pb-mapper
sudo install -m 0644 services/pb-mapper-server.service /etc/systemd/system/
sudo install -m 0644 services/pb-mapper-register@.service /etc/systemd/system/
sudo install -m 0644 services/pb-mapper-connect@.service /etc/systemd/system/
```

The relay unit runs `pb-mapper server` directly. On first start it creates a
random administrator key at `/var/lib/pb-mapper/auth/admin.key` with mode
`0600`. Keep that directory persistent. If upgrading from the old
machine-derived mode, the first v0.4 start automatically copies
`/var/lib/pb-mapper-server/msg_header_key` to the new path when neither an
administrator key file nor `MSG_HEADER_KEY` is already configured.

Use the administrator key locally for management, then issue a scoped temporary
credential for each tenant or workload:

```bash
export MSG_HEADER_KEY="$(sudo cat /var/lib/pb-mapper/auth/admin.key)"
pb-mapper admin --server relay.example.com:7666 key issue --ttl 24h --label home-web
```

Registration instances read `/etc/pb-mapper/register/<name>.env`:

```ini
PB_MAPPER_SERVER=relay.example.com:7666
SERVICE_KEY=home-web
LOCAL_ADDR=127.0.0.1:8080
TRANSPORT=tcp
REGISTER_EXTRA_ARGS=--codec --keep-alive
MSG_HEADER_KEY=pbmt1_replace-with-an-issued-temporary-credential
```

Connect instances read `/etc/pb-mapper/connect/<name>.env`:

```ini
PB_MAPPER_SERVER=relay.example.com:7666
SERVICE_KEY=home-web
LOCAL_ADDR=127.0.0.1:9090
TRANSPORT=tcp
CONNECT_EXTRA_ARGS=--keep-alive
MSG_HEADER_KEY=pbmt1_replace-with-the-same-temporary-credential
```

Create the matching directory and env file, then enable the instance:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now pb-mapper-register@home-web.service
sudo systemctl enable --now pb-mapper-connect@home-web.service
```
