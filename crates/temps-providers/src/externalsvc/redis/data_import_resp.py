# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Copy the keys of one Redis logical database into another, key by key.
#
# Run as two halves joined by a pipe, so a failure is attributed to the side
# that caused it (see `data_import::runner::compose_script`):
#
#   python3 -c "$SCRIPT" produce   # source: SCAN + DUMP + PTTL -> RESP RESTOREs on stdout
#   python3 -c "$SCRIPT" consume   # target: RESP RESTOREs on stdin -> SELECT db, pipeline
#
# DUMP/RESTORE is used rather than replication (SYNC/PSYNC) or an RDB file
# because hosted Redis services commonly disable replication commands but
# allow DUMP. Values travel as opaque, binary-safe serialized payloads with
# their remaining TTL. Standard library only: nothing is installed at run time.
#
# Configuration comes from TEMPS_IMPORT_* environment variables; secrets are
# never printed.

import os
import socket
import ssl
import sys

SCAN_COUNT = 1000
# Commands sent to the target before replies are read back.
TARGET_BATCH = 500
TARGET_BATCH_BYTES = 8 * 1024 * 1024


class RedisError(object):
    def __init__(self, message):
        self.message = message

    def __str__(self):
        return self.message


def fail(side, message):
    sys.stderr.write("temps-import: %s: %s\n" % (side, message))
    sys.stderr.flush()
    sys.exit(1)


def encode(*parts):
    out = [b"*%d\r\n" % len(parts)]
    for part in parts:
        if isinstance(part, int):
            part = str(part).encode()
        elif isinstance(part, str):
            part = part.encode()
        out.append(b"$%d\r\n" % len(part))
        out.append(part)
        out.append(b"\r\n")
    return b"".join(out)


class Reader(object):
    """Incremental RESP parser over a recv(n) callable."""

    def __init__(self, recv):
        self.recv = recv
        self.buf = bytearray()
        self.pos = 0

    def _fill(self):
        if self.pos:
            del self.buf[: self.pos]
            self.pos = 0
        chunk = self.recv(65536)
        if not chunk:
            raise EOFError("connection closed")
        self.buf += chunk

    def at_eof(self):
        if self.pos < len(self.buf):
            return False
        try:
            self._fill()
        except EOFError:
            return True
        return False

    def _line(self):
        while True:
            end = self.buf.find(b"\r\n", self.pos)
            if end >= 0:
                line = bytes(self.buf[self.pos : end])
                self.pos = end + 2
                return line
            self._fill()

    def _exact(self, size):
        while len(self.buf) - self.pos < size + 2:
            self._fill()
        data = bytes(self.buf[self.pos : self.pos + size])
        self.pos += size + 2
        return data

    def reply(self):
        line = self._line()
        kind, rest = line[:1], line[1:]
        if kind == b"+":
            return rest
        if kind == b"-":
            return RedisError(rest.decode("utf-8", "replace"))
        if kind == b":":
            return int(rest)
        if kind == b"$":
            size = int(rest)
            return None if size < 0 else self._exact(size)
        if kind == b"*":
            size = int(rest)
            return None if size < 0 else [self.reply() for _ in range(size)]
        raise ValueError("unexpected RESP type %r" % kind)


def show(key):
    text = repr(key)
    return text if len(text) <= 80 else text[:77] + "..."


def connect(side, host, port, tls, verify):
    try:
        sock = socket.create_connection((host, int(port)), timeout=30)
    except OSError as error:
        fail(side, "could not connect to %s:%s: %s" % (host, port, error))
    if tls:
        context = ssl.create_default_context()
        if not verify:
            context.check_hostname = False
            context.verify_mode = ssl.CERT_NONE
        try:
            sock = context.wrap_socket(sock, server_hostname=host)
        except (OSError, ssl.SSLError) as error:
            fail(side, "TLS handshake with %s:%s failed: %s" % (host, port, error))
    sock.settimeout(300)
    return sock, Reader(sock.recv)


def call(side, sock, reader, *args):
    sock.sendall(encode(*args))
    reply = reader.reply()
    if isinstance(reply, RedisError):
        fail(side, "%s failed: %s" % (args[0], reply))
    return reply


def authenticate(side, sock, reader, user, password):
    if not password:
        return
    if user:
        call(side, sock, reader, "AUTH", user, password)
    else:
        call(side, sock, reader, "AUTH", password)


def produce():
    env = os.environ
    side = "source"
    sock, reader = connect(
        side,
        env["TEMPS_IMPORT_SOURCE_HOST"],
        env["TEMPS_IMPORT_SOURCE_PORT"],
        env.get("TEMPS_IMPORT_SOURCE_TLS") == "1",
        env.get("TEMPS_IMPORT_SOURCE_TLS_VERIFY", "1") == "1",
    )
    authenticate(
        side,
        sock,
        reader,
        env.get("TEMPS_IMPORT_SOURCE_USER", ""),
        env.get("TEMPS_IMPORT_SOURCE_PASSWORD", ""),
    )
    database = int(env.get("TEMPS_IMPORT_SOURCE_DATABASE", "0"))
    if database:
        call(side, sock, reader, "SELECT", database)

    out = sys.stdout.buffer
    cursor = b"0"
    copied = 0
    while True:
        reply = call(side, sock, reader, "SCAN", cursor, "COUNT", SCAN_COUNT)
        cursor, keys = reply[0], reply[1]
        if keys:
            sock.sendall(b"".join(encode("DUMP", k) + encode("PTTL", k) for k in keys))
            for key in keys:
                payload = reader.reply()
                ttl = reader.reply()
                for result in (payload, ttl):
                    if isinstance(result, RedisError):
                        fail(side, "reading key %s failed: %s" % (show(key), result))
                # Deleted or expired between SCAN and DUMP: nothing to copy.
                if payload is None or ttl == -2:
                    continue
                # REPLACE: SCAN may return a key twice.
                out.write(encode("RESTORE", key, max(ttl, 0), payload, "REPLACE"))
                copied += 1
            if copied and copied % 50000 < len(keys):
                sys.stderr.write("temps-import: read %d keys\n" % copied)
        if cursor == b"0":
            break
    out.flush()
    sys.stderr.write("temps-import: read %d keys from the source\n" % copied)


def consume():
    env = os.environ
    side = "target"
    sock, reader = connect(
        side,
        env["TEMPS_IMPORT_TARGET_HOST"],
        env["TEMPS_IMPORT_TARGET_PORT"],
        False,
        False,
    )
    authenticate(side, sock, reader, "", env.get("TEMPS_IMPORT_TARGET_PASSWORD", ""))
    call(side, sock, reader, "SELECT", int(env["TEMPS_IMPORT_TARGET_DATABASE"]))

    source = Reader(sys.stdin.buffer.read1)
    batch, keys, size, written = [], [], 0, 0

    def flush():
        sock.sendall(b"".join(batch))
        for key in keys:
            reply = reader.reply()
            if isinstance(reply, RedisError):
                fail(side, "restoring key %s failed: %s" % (show(key), reply))

    while not source.at_eof():
        command = source.reply()
        if not isinstance(command, list) or len(command) < 2:
            fail(side, "unexpected input from the source side")
        encoded = encode(*command)
        batch.append(encoded)
        keys.append(command[1])
        size += len(encoded)
        if len(batch) >= TARGET_BATCH or size >= TARGET_BATCH_BYTES:
            flush()
            written += len(batch)
            batch, keys, size = [], [], 0
    if batch:
        flush()
        written += len(batch)
    sys.stderr.write("temps-import: restored %d keys into the target\n" % written)


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode == "produce":
        produce()
    elif mode == "consume":
        consume()
    else:
        fail("helper", "expected 'produce' or 'consume', got %r" % mode)
