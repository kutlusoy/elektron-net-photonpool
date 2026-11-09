#!/usr/bin/env python3
"""Simulated SHA-256d miner for a Photon Pool (Stratum V1), standard library only.

It behaves like firmware of the cgminer family: it follows the extranonce size the pool announces (0 = header only mining, otherwise it builds the coinbase from
coinb1 + extranonce1 + extranonce2 + coinb2 and counts extranonce2), asks for a low share difficulty, hashes in pure Python (a few hundred kH/s) and submits the
shares it finds. Use it to see whether the pool accepts a miner of a given user agent; a script that does not hash can never deliver a share.

  python3 sim_asic.py HOST PORT ADDRESS.WORKER [--agent bmminer/2.0.0] [--seconds 120] [--difficulty 0.0005] [--password x]
"""
import argparse, hashlib, json, socket, struct, sys, time

def sha256d(b): return hashlib.sha256(hashlib.sha256(b).digest()).digest()

def swap_words(h):
    b = bytes.fromhex(h)
    return b"".join(b[i:i + 4][::-1] for i in range(0, 32, 4))

def target_of(diff):
    return int(0xffff * 2 ** 208 / diff)

class Conn:
    def __init__(self, host, port):
        self.s = socket.create_connection((host, port), timeout=10)
        self.f = self.s.makefile("rwb", buffering=0)
        self.id = 0
        self.pending = []
    def send(self, method, params):
        self.id += 1
        self.f.write((json.dumps({"id": self.id, "method": method, "params": params}) + "\n").encode())
        return self.id
    def read(self, timeout=None):
        self.s.settimeout(timeout)
        line = self.f.readline()
        if not line: raise ConnectionError("closed by the pool")
        return json.loads(line)
    def call(self, method, params):
        rid = self.send(method, params)
        while True:
            m = self.read(10)
            if m.get("id") == rid and "method" not in m: return m
            self.pending.append(m)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("host"); ap.add_argument("port", type=int); ap.add_argument("user")
    ap.add_argument("--agent", default="bmminer/2.0.0"); ap.add_argument("--seconds", type=int, default=120)
    ap.add_argument("--difficulty", type=float, default=0.0005); ap.add_argument("--password", default="x")
    a = ap.parse_args()
    c = Conn(a.host, a.port)
    cfg = c.call("mining.configure", [["version-rolling"], {"version-rolling.mask": "1fffe000", "version-rolling.min-bit-count": 2}])
    mask = int(cfg["result"].get("version-rolling.mask", "0"), 16) if cfg["result"].get("version-rolling") else 0
    sub = c.call("mining.subscribe", [a.agent])["result"]
    en1, n2size = bytes.fromhex(sub[1]), int(sub[2])
    print(f"subscribe: extranonce1 {sub[1]!r}, extranonce2_size {n2size}, version mask {mask:08x}  ->  {'extranonce mode' if n2size else 'header only mining'}")
    ok = c.call("mining.authorize", [a.user, a.password])["result"]
    print("authorize:", ok)
    if not ok: return 1
    c.send("mining.suggest_difficulty", [a.difficulty])
    diff, job, en2 = a.difficulty, None, 0
    stats = {"accepted": 0, "rejected": 0, "hashes": 0}
    t_end, t0 = time.time() + a.seconds, time.time()
    nonce = 0

    def handle(m):
        nonlocal diff, job, en2, nonce
        if m.get("method") == "mining.set_difficulty":
            diff = float(m["params"][0]); print(f"pool difficulty {diff:g}")
        elif m.get("method") == "mining.notify":
            job = m["params"]; nonce = 0
        elif "result" in m and m.get("id", 0) > 3:
            if m["result"] is True: stats["accepted"] += 1; print(f"share accepted ({stats['accepted']})")
            else: stats["rejected"] += 1; print("share rejected:", m.get("error"))

    c.s.setblocking(False)
    buf = b""
    while time.time() < t_end:
        try:
            data = c.s.recv(65536)
            if not data: print("pool closed the connection"); break
            buf += data
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if line.strip(): handle(json.loads(line))
        except BlockingIOError:
            pass
        if job is None: time.sleep(0.05); continue
        jid, prev, c1, c2, branch, ver, bits, ntime, clean = job
        if clean: en2 = 0
        tgt = target_of(diff)
        ext = b""
        if n2size:
            ext = en1 + struct.pack("<Q", en2)[:n2size]
        cb = bytes.fromhex(c1) + ext + bytes.fromhex(c2)
        root = sha256d(cb)
        for b in branch: root = sha256d(root + bytes.fromhex(b))
        head = struct.pack("<I", int(ver, 16)) + swap_words(prev) + root + struct.pack("<I", int(ntime, 16)) + struct.pack("<I", int(bits, 16))
        end = nonce + 20000
        found = None
        while nonce < end:
            h = sha256d(head + struct.pack("<I", nonce))
            if int.from_bytes(h[::-1], "big") <= tgt: found = nonce; nonce += 1; break
            nonce += 1
        stats["hashes"] += 20000 if found is None else 1
        if found is not None:
            params = [a.user, jid, ext[len(en1):].hex() if n2size else "", ntime, f"{found:08x}"]
            c.s.setblocking(True); c.send("mining.submit", params); c.s.setblocking(False)
        if nonce >= 2 ** 32 - 20000:
            nonce = 0; en2 += 1
    el = time.time() - t0
    print(f"done after {el:.0f} s: {stats['accepted']} shares accepted, {stats['rejected']} rejected, about {stats['hashes'] / el / 1e3:.0f} kH/s")
    return 0

sys.exit(main())
