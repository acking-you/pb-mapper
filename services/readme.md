# systemd units

Install the unified binary and the unit you need:

```bash
sudo install -m 0755 target/release/pb-mapper /usr/local/bin/pb-mapper
sudo install -m 0644 services/pb-mapper-server.service /etc/systemd/system/
sudo install -m 0644 services/pb-mapper-register@.service /etc/systemd/system/
sudo install -m 0644 services/pb-mapper-connect@.service /etc/systemd/system/
```

The relay unit runs `pb-mapper server` directly. Override it with a systemd
drop-in if the default `7666` port or machine-derived key behavior is not
appropriate.

Registration instances read `/etc/pb-mapper/register/<name>.env`:

```ini
PB_MAPPER_SERVER=relay.example.com:7666
SERVICE_KEY=home-web
LOCAL_ADDR=127.0.0.1:8080
TRANSPORT=tcp
REGISTER_EXTRA_ARGS=--codec --keep-alive
MSG_HEADER_KEY=replace-with-the-shared-32-byte-key
```

Connect instances read `/etc/pb-mapper/connect/<name>.env`:

```ini
PB_MAPPER_SERVER=relay.example.com:7666
SERVICE_KEY=home-web
LOCAL_ADDR=127.0.0.1:9090
TRANSPORT=tcp
CONNECT_EXTRA_ARGS=--keep-alive
MSG_HEADER_KEY=replace-with-the-shared-32-byte-key
```

Create the matching directory and env file, then enable the instance:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now pb-mapper-register@home-web.service
sudo systemctl enable --now pb-mapper-connect@home-web.service
```
