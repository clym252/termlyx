#!/usr/bin/env python3
"""Mac side of the `fixture` step's docker relay. See scripts/win/README.md.

The fixture-backed tests shell out to `docker` to reach into the fixture
container. On the VM that `docker` is a shim (docker-shim.cs) that sends its
argv here over the step's reverse SSH tunnel; this relay runs the matching
command against the Mac's Docker and sends back the exit code and output.

Only the two shapes the tests use are accepted:

    docker exec tether-sshd <args...>
    docker compose -f <...>/tests/fixtures/docker-compose.yml restart sshd

The compose file path is replaced with the Mac's copy. Anything else is
refused with exit code 125.

Usage: docker-relay.py <unix socket path> <repo root>

Wire format (all integers big-endian):
    request:  u32 argc, then per arg: u32 len + UTF-8 bytes
    response: i32 exit code, u32 len + stdout, u32 len + stderr
"""

import os
import socketserver
import struct
import subprocess
import sys
import threading
import time

CONTAINER = "tether-sshd"
COMPOSE_SUFFIX = "tests/fixtures/docker-compose.yml"
MAX_ARGS = 256
MAX_ARG_BYTES = 1 << 20


def read_exact(f, n):
    data = f.read(n)
    if len(data) != n:
        raise EOFError("short read")
    return data


def read_u32(f):
    return struct.unpack(">I", read_exact(f, 4))[0]


def command_for(argv, compose_file):
    """Returns the docker argv to run, or None if `argv` is not allowed."""
    if len(argv) >= 3 and argv[0] == "exec" and argv[1] == CONTAINER:
        return ["docker", *argv]
    if (
        len(argv) == 5
        and argv[0] == "compose"
        and argv[1] == "-f"
        and os.path.normpath(argv[2].replace("\\", "/")).endswith(COMPOSE_SUFFIX)
        and argv[3:] == ["restart", "sshd"]
    ):
        return ["docker", "compose", "-f", compose_file, "restart", "sshd"]
    return None


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        try:
            argc = read_u32(self.rfile)
            if argc > MAX_ARGS:
                raise ValueError("too many args")
            argv = []
            for _ in range(argc):
                n = read_u32(self.rfile)
                if n > MAX_ARG_BYTES:
                    raise ValueError("arg too long")
                argv.append(read_exact(self.rfile, n).decode("utf-8"))
        except (EOFError, ValueError, UnicodeDecodeError) as e:
            self.reply(125, b"", f"docker-relay: bad request: {e}\n".encode())
            return

        cmd = command_for(argv, self.server.compose_file)
        if cmd is None:
            # Name the subcommand only; the arguments may be long scripts.
            what = " ".join(argv[:2])
            self.reply(125, b"", f"docker-relay: not allowed: docker {what} ...\n".encode())
            return
        out = subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True)
        self.reply(out.returncode, out.stdout, out.stderr)

    def reply(self, code, stdout, stderr):
        self.wfile.write(struct.pack(">iI", code, len(stdout)) + stdout)
        self.wfile.write(struct.pack(">I", len(stderr)) + stderr)


class Server(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True


def exit_with_parent(server):
    """Stops the relay if win.sh dies without running its cleanup trap."""
    parent = os.getppid()
    while os.getppid() == parent:
        time.sleep(2)
    server.shutdown()


def main():
    if len(sys.argv) != 3:
        print("usage: docker-relay.py <unix socket path> <repo root>", file=sys.stderr)
        sys.exit(2)
    sock, root = sys.argv[1], sys.argv[2]
    try:
        with Server(sock, Handler) as server:
            server.compose_file = os.path.join(root, COMPOSE_SUFFIX)
            threading.Thread(target=exit_with_parent, args=(server,), daemon=True).start()
            server.serve_forever()
    finally:
        try:
            os.unlink(sock)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    main()
