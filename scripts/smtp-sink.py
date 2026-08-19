#!/usr/bin/env python3
"""A single-purpose SMTP sink: accept mail, write each message to a file, never deliver anything.

Used by `scripts/binary-smoke.sh` as the smarthost `amkd --role api` relays through, so the gate
can assert on the bytes that actually went ON THE WIRE rather than on anything the sender claims
about them. That distinction is the whole point of the gate: `amk-outbound` was fully implemented
and unit-tested while the deployed binary could not sign a single message, because every existing
test inspected the library rather than the wire.

Deliberately not `aiosmtpd` or any other dependency -- this speaks the six verbs a relay needs and
nothing else, so the gate adds no install step and works with a cold cache and no network.

    python3 scripts/smtp-sink.py --port 52525 --outdir /tmp/sink   # writes msg-0001.eml, ...
    python3 scripts/smtp-sink.py --port 52525 --outdir /tmp/sink --cert c.pem --key k.pem

STARTTLS is offered when --cert/--key are given, and this is not optional decoration:
`amk-outbound::smtp::deliver_to_host` falls back to plaintext ONLY on port 25, so a smarthost on
any other port must complete a TLS handshake or the send fails with "STARTTLS extension
unavailable". Serving it here means the gate exercises the real production delivery path rather
than a plaintext shortcut no deployment uses. A self-signed certificate is fine because the
sender sets `allow_invalid_certs()`.

Binds loopback only. It accepts anything from anyone, which is safe exactly because it is
loopback, ephemeral, and delivers nowhere.
"""
import argparse
import os
import socket
import socketserver
import ssl
import sys
import threading

count = 0
count_lock = threading.Lock()


class Handler(socketserver.StreamRequestHandler):
    timeout = 30

    def reply(self, code, text):
        self.wfile.write(f"{code} {text}\r\n".encode())
        self.wfile.flush()

    def handle(self):
        global count
        self.tls = False
        self.reply(220, "amk-smtp-sink ready")
        while True:
            line = self.rfile.readline()
            if not line:
                return
            verb = line.decode("utf-8", "replace").strip()
            up = verb.upper()
            if up.startswith("EHLO"):
                lines = [b"250-amk-smtp-sink\r\n", b"250-8BITMIME\r\n"]
                # Offer STARTTLS only while still in the clear: re-advertising it after the
                # handshake is what RFC 3207 forbids, and mail-send notices.
                if self.server.tls_ctx is not None and not self.tls:
                    lines.append(b"250-STARTTLS\r\n")
                lines.append(b"250 SIZE 26214400\r\n")
                self.wfile.write(b"".join(lines))
                self.wfile.flush()
            elif up.startswith("STARTTLS"):
                if self.server.tls_ctx is None or self.tls:
                    self.reply(454, "TLS not available")
                    continue
                self.reply(220, "Ready to start TLS")
                try:
                    self.connection = self.server.tls_ctx.wrap_socket(
                        self.connection, server_side=True
                    )
                except ssl.SSLError as e:
                    print(f"sink: TLS handshake failed: {e}", flush=True)
                    return
                # Post-handshake the session resets to its initial state (RFC 3207 s4), so the
                # buffers must be rebuilt over the encrypted socket -- reusing the old rfile here
                # reads ciphertext as commands and the session dies with a confusing 502.
                self.rfile = self.connection.makefile("rb", -1)
                self.wfile = self.connection.makefile("wb", 0)
                self.tls = True
            elif up.startswith("HELO"):
                self.reply(250, "amk-smtp-sink")
            elif up.startswith(("MAIL", "RCPT")):
                self.reply(250, "OK")
            elif up.startswith("DATA"):
                self.reply(354, "End data with <CR><LF>.<CR><LF>")
                chunks = []
                while True:
                    d = self.rfile.readline()
                    if not d or d in (b".\r\n", b".\n"):
                        break
                    # Undo transparency: a body line starting with '.' was doubled by the sender.
                    chunks.append(d[1:] if d.startswith(b"..") else d)
                with count_lock:
                    count += 1
                    n = count
                path = os.path.join(self.server.outdir, f"msg-{n:04d}.eml")
                with open(path, "wb") as fh:
                    fh.write(b"".join(chunks))
                print(
                    f"sink: captured {path} ({sum(len(c) for c in chunks)} bytes, "
                    f"tls={self.tls})",
                    flush=True,
                )
                self.reply(250, f"OK queued as {n}")
            elif up.startswith("RSET"):
                self.reply(250, "OK")
            elif up.startswith("NOOP"):
                self.reply(250, "OK")
            elif up.startswith("QUIT"):
                self.reply(221, "Bye")
                return
            else:
                self.reply(502, "Command not implemented")


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
    address_family = socket.AF_INET


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--outdir", required=True)
    ap.add_argument("--cert", help="PEM certificate; enables STARTTLS when given with --key")
    ap.add_argument("--key", help="PEM private key")
    a = ap.parse_args()
    os.makedirs(a.outdir, exist_ok=True)
    tls_ctx = None
    if a.cert and a.key:
        tls_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls_ctx.load_cert_chain(a.cert, a.key)
    srv = Server(("127.0.0.1", a.port), Handler)
    srv.outdir = a.outdir
    srv.tls_ctx = tls_ctx
    print(
        f"sink: listening on 127.0.0.1:{a.port} -> {a.outdir} "
        f"(starttls={'yes' if tls_ctx else 'no'})",
        flush=True,
    )
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
