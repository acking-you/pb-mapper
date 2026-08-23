---
name: pb-mapper-suite
description: Install or upgrade the pb-mapper CLI without a repository checkout, deploy its public relay, configure register and connect roles, manage scoped credentials, and verify tunnels end to end.
---

# pb-mapper Suite

Install and operate pb-mapper directly from official release artifacts. Never
require the user to clone the pb-mapper repository, and never depend on files
from a checkout.

Official sources:

- releases: `https://github.com/acking-you/pb-mapper/releases`
- latest-release API:
  `https://api.github.com/repos/acking-you/pb-mapper/releases/latest`

## Start with three confirmations

Before downloading, connecting over SSH, or changing a machine, ask the user to
confirm:

1. The relay's public TCP port, default `7666`, is fully open from the Internet
   in both the cloud security group and the host firewall. Do not change
   firewall rules unless the user explicitly asks.
2. The server OS and architecture. Offer **Linux x86_64** as the default, mapped
   to `x86_64-unknown-linux-musl`, but require confirmation before selecting an
   artifact.
3. The SSH target and port, plus whether `sudo` is available.

Stop if any answer is missing or contradictory. Later, detect the OS and
architecture of every additional register/connect host independently.

## Install or upgrade the unified CLI

1. Inspect the target with read-only commands: OS, architecture, current
   `pb-mapper --version`, existing services, listeners, configuration, and auth
   state. Never print credentials.
2. Query the latest-release API and select the asset whose target exactly
   matches the detected platform. The archive naming convention is
   `pb-mapper-<target>.tar.gz`; Windows assets may use `.zip`. Do not guess an
   unsupported target or reuse the server artifact on another platform.
3. Download to a temporary directory on the target. If the target cannot reach
   GitHub, download on the operator machine and transfer the archive over SSH.
   Validate the archive and any published checksum, extract only the expected
   `pb-mapper` executable, then install it in the platform's normal executable
   path (`/usr/local/bin/pb-mapper` with mode `0755` on a managed Unix host).
4. Run `pb-mapper --version` and `pb-mapper -h`. The installed binary's help is
   the source of truth for all role flags and nested commands.

For an upgrade, preserve a recoverable copy of the existing binary and retain
all configuration, units, and authentication state until validation succeeds.
Restart only the pb-mapper roles affected by the request.

## Deploy or upgrade the relay

Use `pb-mapper server -h` to build the command for the confirmed port. On a
Linux server, create a minimal systemd service with these properties:

- run `/usr/local/bin/pb-mapper server` with the confirmed port;
- start after the network is online;
- set `RUST_LOG=info`;
- use `StateDirectory=pb-mapper` with mode `0700`, so authentication state lives
  persistently under `/var/lib/pb-mapper/auth`;
- restart on failure with a short delay; and
- enable the service at boot.

Never overwrite or remove `/var/lib/pb-mapper/auth` during an upgrade. After
starting the relay, confirm the service is active and enabled, its restart count
is stable, and the expected TCP port is listening. Probe that port from outside
the server; the user's firewall confirmation is not operationally verified
until the external probe succeeds.

The relay creates or uses one administrator key in its auth state directory.
Keep it on the relay, readable only by root. Supply it to local administrator
commands through `MSG_HEADER_KEY` without printing it, then use
`pb-mapper admin -h` and nested help to issue a scoped `pbmt1_` temporary
credential for each workload. Never give the administrator key to ordinary
register or connect hosts.

## Register and connect

Install the same unified CLI on each participating host. Put the same scoped
temporary credential in `MSG_HEADER_KEY` on both sides of one tunnel namespace,
without logging it or placing it in shell history.

Use these examples as intent, then derive the exact command from the installed
binary's help:

| Role | Example | Required help |
| --- | --- | --- |
| Registration side | Publish private TCP service `127.0.0.1:8080` as `agent-control` | `pb-mapper register -h` |
| Connection side | Expose `agent-control` locally at `127.0.0.1:18080` | `pb-mapper connect -h` |
| Inspection | Confirm the scoped namespace contains `agent-control` | `pb-mapper status -h` |

Default the connect-side listener to `127.0.0.1`. Bind it publicly only when the
user explicitly requests that exposure and confirms its firewall and access
control. For persistent Linux register/connect roles, wrap the help-derived
command in a minimal systemd service, keep the credential in a root-readable
environment file with mode `0600`, and verify the service does not restart-loop.

## Completion

Do not stop at process startup. Verify:

- every host reports the intended pb-mapper version;
- the relay is externally reachable on the confirmed port;
- scoped status shows the registered service;
- the connect-side address is listening;
- a real payload round-trip reaches the private service; and
- all managed units remain healthy with stable restart counts.

Report the version, selected release targets, relay address, firewall result,
managed roles, and end-to-end result. Redact every credential. If an upgrade
fails, restore the preserved binary/configuration and identify the failing
check.
