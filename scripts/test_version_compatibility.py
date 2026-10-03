#!/usr/bin/env python3
"""Exercise old/new relay, register and connect binaries on loopback only.

Build the compatibility_relay example against each revision, retaining both
artifacts. No installed binary, credential, route or existing service is changed.
"""

import argparse
import contextlib
import itertools
import os
from pathlib import Path
import selectors
import socket
import socketserver
import subprocess
import tempfile
import threading
import time


class TcpEcho(socketserver.BaseRequestHandler):
    def handle(self):
        while data := self.request.recv(65536):
            self.request.sendall(data)


class UdpEcho(socketserver.BaseRequestHandler):
    def handle(self):
        data, channel = self.request
        channel.sendto(data, self.client_address)


class TcpServer(socketserver.ThreadingTCPServer):
    daemon_threads = True


@contextlib.contextmanager
def child(command, env, *, capture=False):
    process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, text=True)
    try:
        yield process
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)


def relay_address(process):
    with selectors.DefaultSelector() as ready:
        ready.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if ready.select(timeout=0.1):
                line = process.stdout.readline()
                if line.startswith("COMPAT_RELAY="):
                    return line.strip().split("=", 1)[1]
            if process.poll() is not None:
                raise RuntimeError("compatibility relay exited before readiness")
    raise TimeoutError("compatibility relay readiness")


def probe(port, udp, processes):
    deadline = time.monotonic() + 10
    last_error = None
    while time.monotonic() < deadline:
        if any(process.poll() is not None for process in processes):
            raise RuntimeError("compatibility child exited before forwarding")
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM if udp else socket.SOCK_STREAM) as client:
                client.settimeout(0.5)
                client.connect(("127.0.0.1", port))
                for length in (1, 123, 1199) if udp else (1, 123, 32000):
                    payload = bytes(index % 251 for index in range(length))
                    client.sendall(payload)
                    response = client.recv(65536)
                    while not udp and len(response) < length:
                        tail = client.recv(length - len(response))
                        if not tail:
                            raise ConnectionError("unexpected EOF")
                        response += tail
                    if response != payload:
                        raise AssertionError("payload or datagram boundary changed")
                return
        except (OSError, ConnectionError) as error:
            last_error = error
            time.sleep(0.05)
    raise TimeoutError(f"forwarding readiness: {last_error}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("old", "new", "old-relay", "new-relay"):
        parser.add_argument(f"--{option}", type=Path, required=True)
    args = parser.parse_args()
    binaries = [str(args.old.resolve()), str(args.new.resolve())]
    relays = [str(args.old_relay.resolve()), str(args.new_relay.resolve())]
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("PB_MAPPER_") and key not in ("MSG_HEADER_KEY", "RUST_LOG")}
    env.update(MSG_HEADER_KEY="0123456789abcdefghijklmnopqrstuv", RUST_LOG="off")
    for relay, register, connect, udp, codec in itertools.product(range(2), range(2), range(2), (False, True), (False, True)):
        with tempfile.TemporaryDirectory(prefix="pb-mapper-compat-") as state, contextlib.ExitStack() as stack:
            server = stack.enter_context((socketserver.UDPServer if udp else TcpServer)(
                ("127.0.0.1", 0), UdpEcho if udp else TcpEcho))
            worker = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
            worker.start()
            stack.callback(worker.join, 3)
            stack.callback(server.shutdown)
            process = stack.enter_context(child([relays[relay], state], env, capture=True))
            address = relay_address(process)
            # CLI listeners cannot inherit a pre-bound socket; select an OS-assigned
            # port immediately before startup and fail on a colliding bind.
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM if udp else socket.SOCK_STREAM) as reserved:
                reserved.bind(("127.0.0.1", 0))
                port = reserved.getsockname()[1]
            transport = "udp" if udp else "tcp"
            common = [transport, "--key", "compat", "--server", address]
            publisher = stack.enter_context(child([binaries[register], "register", *common, "--addr",
                                                  f"127.0.0.1:{server.server_address[1]}", *(["--codec"] if codec else [])], env))
            subscriber = stack.enter_context(child([binaries[connect], "connect", *common, "--addr", f"127.0.0.1:{port}"], env))
            probe(port, udp, [process, publisher, subscriber])
            print(f"PASS relay={'new' if relay else 'old'} register={'new' if register else 'old'} "
                  f"connect={'new' if connect else 'old'} transport={transport} codec={codec}", flush=True)


if __name__ == "__main__":
    main()
