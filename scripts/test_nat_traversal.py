#!/usr/bin/env python3
"""Linux acceptance: two NATs per endpoint, changed mappings, IPv6 and relay.

Run as root with iproute2 and iptables installed. Everything, including the
relay and gateway rules, lives in temporary network namespaces. Only logs are
retained; credentials are generated per scenario and never written to logs.
"""

import argparse
import hashlib
import ipaddress
import json
import os
import secrets
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

SCRIPT = str(Path(__file__).resolve())


def command(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs)


def wait_for(check, description, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.05)
    raise TimeoutError(description)


def worker(kind, args):
    if kind == "echo":
        with socket.socket() as server:
            server.bind(("127.0.0.1", 18080))
            server.listen()
            print("ready", flush=True)
            while True:
                conn, _ = server.accept()
                print("backend_accept", flush=True)
                with conn:
                    while data := conn.recv(65536):
                        conn.sendall(data)
    elif kind == "udp-echo":
        with socket.socket(type=socket.SOCK_DGRAM) as server:
            server.bind(("127.0.0.1", 18081))
            print("ready", flush=True)
            while True:
                data, peer = server.recvfrom(65536)
                server.sendto(data, peer)
    elif kind == "check":
        payload = bytes(range(256)) * 256
        with socket.socket() as conn:

            def connect():
                try:
                    conn.connect(("127.0.0.1", 18090))
                    return True
                except ConnectionRefusedError:
                    return False

            wait_for(connect, "consumer listener did not bind")
            conn.settimeout(15)
            conn.sendall(payload)
            conn.shutdown(socket.SHUT_WR)
            received = b""
            while data := conn.recv(65536):
                received += data
            assert received == payload, "TCP payload or half-close lost"
        print(f"TCP {len(payload)} bytes sha256={hashlib.sha256(payload).hexdigest()}")
    elif kind == "udp-check":
        payload = bytes(range(200)) * 6
        with socket.socket(type=socket.SOCK_DGRAM) as conn:
            conn.settimeout(1)

            def exchange():
                conn.sendto(payload, ("127.0.0.1", 18091))
                try:
                    assert conn.recv(65536) == payload, "UDP datagram changed"
                    return True
                except TimeoutError:
                    return False

            wait_for(exchange, "UDP round trip failed")
        print(f"UDP {len(payload)} bytes sha256={hashlib.sha256(payload).hexdigest()}")
    elif kind == "gateway":
        gateway(args[0])


def gateway(outer):
    """Test PCP proxy controlling both NATs; not a production PCP server."""
    mappings = {}
    with socket.socket(type=socket.SOCK_DGRAM) as server:
        server.bind(("10.201.3.1", 5351))
        print("ready", flush=True)
        while True:
            request, peer = server.recvfrom(2048)
            if len(request) < 24 or request[0] != 2:
                continue
            opcode = request[1]
            lifetime = struct.unpack_from("!I", request, 4)[0]
            response = bytearray(24)
            response[:4] = bytes([2, 128 | opcode, 0, 0])
            struct.pack_into("!II", response, 4, lifetime, int(time.monotonic()))
            if opcode == 1 and len(request) >= 60:
                # Short leases exercise renewal without a time-based sleep.
                struct.pack_into("!I", response, 4, 2 if lifetime else 0)
                internal = struct.unpack_from("!H", request, 40)[0]
                external = 42000
                if lifetime and internal not in mappings:
                    rules = [
                        (
                            None,
                            [
                                "-t",
                                "nat",
                                "-I",
                                "PREROUTING",
                                "-i",
                                "wan",
                                "-p",
                                "udp",
                                "--dport",
                                str(external),
                                "-j",
                                "DNAT",
                                "--to-destination",
                                f"{peer[0]}:{internal}",
                            ],
                        ),
                        (
                            outer,
                            [
                                "-t",
                                "nat",
                                "-I",
                                "PREROUTING",
                                "-i",
                                "wan",
                                "-p",
                                "udp",
                                "--dport",
                                str(external),
                                "-j",
                                "DNAT",
                                "--to-destination",
                                f"10.201.4.2:{external}",
                            ],
                        ),
                    ]
                    for namespace, rule in rules:
                        prefix = ["ip", "netns", "exec", namespace] if namespace else []
                        command(*prefix, "iptables", *rule)
                    mappings[internal] = rules
                    print(
                        f"mapping_created internal={internal} external={external}",
                        flush=True,
                    )
                elif lifetime:
                    print("mapping_renewed", flush=True)
                elif not lifetime and internal in mappings:
                    for namespace, rule in mappings.pop(internal):
                        prefix = ["ip", "netns", "exec", namespace] if namespace else []
                        command(
                            *prefix,
                            "iptables",
                            *("-D" if arg == "-I" else arg for arg in rule),
                        )
                    print("mapping_deleted", flush=True)
                response.extend(request[24:60])
                struct.pack_into("!H", response, 42, external)
                response[44:60] = ipaddress.IPv6Address("::ffff:198.18.0.3").packed
            elif opcode != 0:
                continue
            server.sendto(response, peer)


class Lab:
    def __init__(self, binary, output, mode):
        self.binary, self.output, self.mode = str(binary), output, mode
        self.names = {
            name: f"pbm-{os.getpid()}-{name}"
            for name in ("c", "c1", "c2", "p", "p1", "p2", "wan")
        }
        self.created, self.processes = [], []
        self.env = dict(
            os.environ,
            MSG_HEADER_KEY=secrets.token_hex(16),
            PB_MAPPER_LOG_FORMAT="json",
            RUST_LOG="info,pb_mapper_client::direct=debug",
        )

    def ip(self, name, *args):
        return command("ip", "-n", self.names[name], *args)

    def run(self, name, *args):
        return command("ip", "netns", "exec", self.names[name], *args, env=self.env)

    def firewall(self, name, *args):
        self.run(name, "iptables", *args)

    def link(self, a, ai, av4, av6, b, bi, bv4, bv6):
        self.ip(
            a,
            "link",
            "add",
            ai,
            "type",
            "veth",
            "peer",
            "name",
            bi,
            "netns",
            self.names[b],
        )
        for name, iface, v4, v6 in ((a, ai, av4, av6), (b, bi, bv4, bv6)):
            self.ip(name, "link", "set", iface, "up")
            if v4:
                self.ip(name, "addr", "add", v4, "dev", iface)
                self.ip(name, "-6", "addr", "add", v6, "dev", iface, "nodad")

    def setup(self):
        self.output.mkdir(parents=True, exist_ok=True)
        for name, namespace in self.names.items():
            command("ip", "netns", "add", namespace)
            self.created.append(namespace)
            self.ip(name, "link", "set", "lo", "up")
            self.run(
                name,
                "sysctl",
                "-qw",
                "net.ipv4.ip_forward=1",
                "net.ipv6.conf.all.forwarding=1",
            )
        self.ip("wan", "link", "add", "br0", "type", "bridge")
        self.ip("wan", "link", "set", "br0", "type", "bridge", "mcast_snooping", "0")
        self.ip("wan", "link", "set", "br0", "up")
        self.ip("wan", "addr", "add", "198.18.0.1/24", "dev", "br0")
        self.ip("wan", "-6", "addr", "add", "2001:db8:0::1/64", "dev", "br0", "nodad")
        for side, subnet, external in (("c", 1, 2), ("p", 3, 3)):
            inner, outer = side + "1", side + "2"
            self.link(
                side,
                "eth0",
                f"10.201.{subnet}.2/24",
                f"2001:db8:{subnet}::2/64",
                inner,
                "lan",
                f"10.201.{subnet}.1/24",
                f"2001:db8:{subnet}::1/64",
            )
            self.link(
                inner,
                "wan",
                f"10.201.{subnet + 1}.2/24",
                f"2001:db8:{subnet + 1}::2/64",
                outer,
                "lan",
                f"10.201.{subnet + 1}.1/24",
                f"2001:db8:{subnet + 1}::1/64",
            )
            self.link(
                outer,
                "wan",
                f"198.18.0.{external}/24",
                f"2001:db8:0::{external}/64",
                "wan",
                side,
                None,
                None,
            )
            self.ip("wan", "link", "set", side, "master", "br0")
            for name, v4, v6 in (
                (side, f"10.201.{subnet}.1", f"2001:db8:{subnet}::1"),
                (inner, f"10.201.{subnet + 1}.1", f"2001:db8:{subnet + 1}::1"),
                (outer, "198.18.0.1", "2001:db8:0::1"),
            ):
                self.ip(name, "route", "add", "default", "via", v4)
                self.ip(name, "-6", "route", "add", "default", "via", v6)
            self.ip(
                outer,
                "-6",
                "route",
                "add",
                f"2001:db8:{subnet}::/64",
                "via",
                f"2001:db8:{subnet + 1}::2",
            )
            self.ip(
                "wan",
                "-6",
                "route",
                "add",
                f"2001:db8:{subnet}::/64",
                "via",
                f"2001:db8:0::{external}",
            )
            for router, destination in (
                (inner, f"10.201.{subnet}.2"),
                (outer, f"10.201.{subnet + 1}.2"),
            ):
                self.firewall(router, "-P", "FORWARD", "DROP")
                self.firewall(
                    router, "-A", "FORWARD", "-i", "lan", "-o", "wan", "-j", "ACCEPT"
                )
                self.firewall(
                    router,
                    "-A",
                    "FORWARD",
                    "-m",
                    "conntrack",
                    "--ctstate",
                    "ESTABLISHED,RELATED",
                    "-j",
                    "ACCEPT",
                )
                self.firewall(
                    router,
                    "-t",
                    "nat",
                    "-A",
                    "POSTROUTING",
                    "-o",
                    "wan",
                    "-j",
                    "MASQUERADE",
                )
                # Explicit port-preserving, endpoint-independent mapping.
                # Bare MASQUERADE may change the port after an early inbound
                # packet reserves a conntrack tuple. Forwarding remains closed
                # until an outbound packet establishes the matching flow.
                self.firewall(
                    router,
                    "-t",
                    "nat",
                    "-A",
                    "PREROUTING",
                    "-i",
                    "wan",
                    "-p",
                    "udp",
                    "-j",
                    "DNAT",
                    "--to-destination",
                    destination,
                )
            if self.mode != "ipv6":
                self.run(side, "ip6tables", "-A", "OUTPUT", "-p", "udp", "-j", "DROP")
        if self.mode == "asymmetric":
            # Consumer permits packets from a newly observed peer port through
            # both layers. Provider uses different ports for STUN and the peer.
            for router, dest in (("c2", "10.201.2.2"), ("c1", "10.201.1.2")):
                self.firewall(
                    router,
                    "-t",
                    "nat",
                    "-A",
                    "PREROUTING",
                    "-i",
                    "wan",
                    "-p",
                    "udp",
                    "-j",
                    "DNAT",
                    "--to-destination",
                    dest,
                )
                self.firewall(router, "-I", "FORWARD", "-p", "udp", "-j", "ACCEPT")
            for destination, port in (("198.18.0.1", 41000), ("198.18.0.2", 41001)):
                self.firewall(
                    "p2",
                    "-t",
                    "nat",
                    "-I",
                    "POSTROUTING",
                    "-o",
                    "wan",
                    "-p",
                    "udp",
                    "-d",
                    destination,
                    "-j",
                    "SNAT",
                    "--to-source",
                    f"198.18.0.3:{port}",
                )
        elif self.mode in ("blocked", "ipv6"):
            self.firewall("c", "-A", "OUTPUT", "-p", "udp", "-j", "DROP")
            if self.mode == "ipv6":
                # Wait for IPv6 routing/neighbor discovery readiness across
                # the new links; ICMP does not establish any UDP permission.
                self.run("c", "ping", "-6", "-c", "1", "-w", "10", "2001:db8:3::2")
        elif self.mode == "mapping":
            for router in ("p1", "p2"):
                self.firewall(router, "-I", "FORWARD", "-p", "udp", "-j", "ACCEPT")
            self.firewall(
                "p",
                "-A",
                "OUTPUT",
                "-p",
                "udp",
                "-m",
                "conntrack",
                "--ctstate",
                "ESTABLISHED",
                "-j",
                "ACCEPT",
            )
            self.firewall(
                "p",
                "-A",
                "OUTPUT",
                "-p",
                "udp",
                "-d",
                "10.201.3.1",
                "--dport",
                "5351",
                "-j",
                "ACCEPT",
            )
            self.firewall("p", "-A", "OUTPUT", "-p", "udp", "-j", "DROP")

    def start(self, name, label, *args):
        log = self.output / f"{label}.log"
        with log.open("w") as output:
            process = subprocess.Popen(
                ["ip", "netns", "exec", self.names[name], *args],
                env=self.env,
                stdout=output,
                stderr=subprocess.STDOUT,
            )
        self.processes.append(process)
        return process

    def log(self, label):
        return (self.output / f"{label}.log").read_text()

    def events(self, label):
        for line in self.log(label).splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue  # The writer may not have finished the last line yet.
            yield event

    def selected(self, label):
        return next(
            (
                event
                for event in self.events(label)
                if event.get("event") == "session_path_selected"
            ),
            {},
        )

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

    def test(self, state):
        if shutil.which("tcpdump"):
            self.start("wan", "packets", "tcpdump", "-n", "-l", "-i", "br0", "udp")
        self.start("wan", "relay", self.binary, "server", "--auth-state-dir", state)
        self.start("p", "backend", sys.executable, SCRIPT, "--worker", "echo")
        self.start("p", "udp-backend", sys.executable, SCRIPT, "--worker", "udp-echo")
        wait_for(lambda: "ready" in self.log("backend"), "backend readiness")
        flags = []
        if self.mode == "mapping":
            self.start(
                "p1",
                "gateway",
                sys.executable,
                SCRIPT,
                "--worker",
                "gateway",
                self.names["p2"],
            )
            wait_for(lambda: "ready" in self.log("gateway"), "PCP gateway readiness")
            flags = ["--port-mapping"]
        provider = self.start(
            "p",
            "provider",
            self.binary,
            "register",
            "tcp",
            "-s",
            "198.18.0.1:7666",
            "-k",
            "echo",
            "-a",
            "127.0.0.1:18080",
            *flags,
        )
        wait_for(
            lambda: any(
                event.get("event") == "local_server_registered"
                for event in self.events("provider")
            ),
            "provider registration readiness",
        )

        def consumer(label):
            return self.start(
                "c",
                label,
                self.binary,
                "connect",
                "tcp",
                "-s",
                "198.18.0.1:7666",
                "-k",
                "echo",
                "-a",
                "127.0.0.1:18090",
            )

        count = 1
        if self.mode == "mapping":
            warmup = consumer("warmup")
            self.run("c", sys.executable, SCRIPT, "--worker", "check")
            wait_for(
                lambda: "mapping_created" in self.log("gateway"), "PCP mapping creation"
            )
            self.stop(warmup)
            self.ip(
                "p", "-6", "addr", "add", "2001:db8:99::1/128", "dev", "eth0", "nodad"
            )
            wait_for(
                lambda: self.log("gateway").count("mapping_created") >= 2,
                "PCP mapping rediscovery after network change",
            )
            wait_for(
                lambda: "mapping_renewed" in self.log("gateway"), "PCP lease renewal"
            )
            count += 1
        client = consumer("consumer")
        result = self.run("c", sys.executable, SCRIPT, "--worker", "check")
        expected = "relay" if self.mode == "blocked" else "direct"
        wait_for(
            lambda: self.selected("consumer").get("path") == expected,
            f"expected {expected} path",
        )
        assert self.log("backend").count("backend_accept") == count, (
            "backend dialed more than once per connection"
        )
        if self.mode == "ipv6":
            assert self.selected("consumer")["peer"].startswith("[2001:db8:"), (
                "IPv6 path was not selected"
            )
        elif self.mode == "asymmetric":
            assert self.selected("consumer")["peer"] == "198.18.0.3:41001", (
                "peer-reflexive mapping was not selected"
            )
        elif self.mode == "mapping":
            assert self.selected("consumer")["peer"] == "198.18.0.3:42000", (
                "PCP mapping was not selected"
            )
        print(
            f"{self.mode}: path={expected}; backend_dials={count}; {result.stdout.strip()}",
            flush=True,
        )
        self.stop(client)
        releases = (
            self.log("gateway").count("mapping_deleted")
            if self.mode == "mapping"
            else 0
        )
        self.stop(provider)
        if self.mode == "mapping":
            wait_for(
                lambda: self.log("gateway").count("mapping_deleted") > releases,
                "PCP mapping release on local stop",
            )
            print(
                "mapping: PCP leases renewed, recreated after network change, and released on stop",
                flush=True,
            )
        if self.mode == "nested":
            self.start(
                "p",
                "udp-provider",
                self.binary,
                "register",
                "udp",
                "-s",
                "198.18.0.1:7666",
                "-k",
                "udp",
                "-a",
                "127.0.0.1:18081",
            )
            wait_for(
                lambda: any(
                    event.get("event") == "local_server_registered"
                    for event in self.events("udp-provider")
                ),
                "UDP provider registration readiness",
            )
            self.start(
                "c",
                "udp-consumer",
                self.binary,
                "connect",
                "udp",
                "-s",
                "198.18.0.1:7666",
                "-k",
                "udp",
                "-a",
                "127.0.0.1:18091",
            )
            result = self.run("c", sys.executable, SCRIPT, "--worker", "udp-check")
            assert self.selected("udp-consumer").get("path") == "direct", (
                "UDP did not go direct"
            )
            print(f"nested: path=direct; {result.stdout.strip()}", flush=True)

    def close(self):
        for process in reversed(self.processes):
            self.stop(process)
        for namespace in reversed(self.created):
            command("ip", "netns", "del", namespace)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/pb-mapper"))
    parser.add_argument("--output", type=Path, default=Path("target/nat-acceptance"))
    parser.add_argument(
        "--case", choices=["nested", "asymmetric", "blocked", "ipv6", "mapping"]
    )
    parser.add_argument(
        "--worker", choices=["echo", "udp-echo", "check", "udp-check", "gateway"]
    )
    parser.add_argument("args", nargs="*")
    args = parser.parse_args()
    if args.worker:
        worker(args.worker, args.args)
        return
    if sys.platform != "linux" or os.geteuid() != 0:
        parser.error("requires Linux root privileges for isolated network namespaces")

    def interrupted(_signal, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    binary = args.binary.resolve(strict=True)
    for mode in (
        [args.case]
        if args.case
        else ["nested", "asymmetric", "blocked", "ipv6", "mapping"]
    ):
        lab = Lab(binary, args.output.resolve() / mode, mode)
        try:
            with tempfile.TemporaryDirectory(prefix="pbm-nat-auth-") as state:
                lab.setup()
                lab.test(state)
        finally:
            lab.close()


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stdout, error.stderr, file=sys.stderr)
        raise
    except KeyboardInterrupt:
        sys.exit(130)
