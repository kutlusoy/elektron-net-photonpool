import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, struct, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, STRATUM = 18981, 18333
NADDR = 8
MINER_ADDR = encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"miner-account").digest()[:20]))

def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]

def write_cfg(path):
    addrs = [encode_segwit_address("bcrt", 0, list(prog(i))) for i in range(1, NADDR + 1)]
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 20

[pool]
network = "regtest"
payout_addresses = {json.dumps(addrs)}
refresh_secs = 0
spot_check = 2

[stratum]
listen = "127.0.0.1:{STRATUM}"
initial_difficulty = 0.001
min_job_interval_ms = 200

[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def get(path): return urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=5).read()

def swap_words(h):
    b = bytes.fromhex(h)
    return b"".join(b[i:i + 4][::-1] for i in range(0, 32, 4))

class Client:
    def __init__(self): self.id = 0; self.pending = []; self.job = None; self.diff = None; self.mask = None
    async def open(self):
        self.r, self.w = await asyncio.open_connection("127.0.0.1", STRATUM)
        return self
    def send(self, method, params):
        self.id += 1
        self.w.write((json.dumps({"id": self.id, "method": method, "params": params}) + "\n").encode())
        return self.id
    async def read(self, timeout=5):
        line = await asyncio.wait_for(self.r.readline(), timeout)
        if not line: raise ConnectionError("closed")
        m = json.loads(line)
        if m.get("method") == "mining.notify": self.job = m["params"]; self.job_time = time.perf_counter()
        elif m.get("method") == "mining.set_difficulty": self.diff = m["params"][0]
        return m
    async def request(self, method, params):
        rid = self.send(method, params)
        while True:
            m = await self.read()
            if m.get("id") == rid and "method" not in m: return m
    async def wait_job(self, clean=None, timeout=5, after=None):
        end = time.perf_counter() + timeout
        while time.perf_counter() < end:
            if self.job and (clean is None or self.job[8] == clean) and (after is None or self.job[0] != after): return self.job
            try: await self.read(timeout=max(0.05, end - time.perf_counter()))
            except asyncio.TimeoutError: break
        return None

def build_header(job, version, nonce, extranonce=b""):
    jid, prev, c1, c2, branch, ver, bits, ntime, clean = job
    cb = bytes.fromhex(c1) + extranonce + bytes.fromhex(c2)
    h = sha256d(cb)
    for s in branch: h = sha256d(h + bytes.fromhex(s))
    return struct.pack("<I", version) + swap_words(prev) + h + struct.pack("<I", int(ntime, 16)) + struct.pack("<I", int(bits, 16)) + struct.pack("<I", nonce)

def bits_target(bits):
    e, m = bits >> 24, bits & 0xffffff
    return m << (8 * (e - 3))

def grind(job, version, want_block=True):
    target = bits_target(int(job[6], 16))
    for nonce in range(1 << 20):
        hdr = build_header(job, version, nonce)
        val = int.from_bytes(sha256d(hdr)[::-1], "big")
        if (val <= target) == want_block: return nonce, sha256d(hdr)[::-1].hex()
    raise RuntimeError("no nonce")

async def main(h):
    n = h.node
    scripts = {(b"\x00\x14" + prog(i)).hex() for i in range(1, NADDR + 1)}
    s = None
    for _ in range(200):
        try:
            s = json.loads(get("/api/v1/summary"))
            if s["alloc"]["candidates"] == NADDR: break
        except Exception: pass
        await asyncio.sleep(0.1)
    h.check("daemon ready with all candidates", s is not None and s["alloc"]["candidates"] == NADDR)

    c = await Client().open()
    r = await c.request("mining.configure", [["version-rolling", "minimum-difficulty"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}])
    h.check("configure grants the 12 miner bits and refuses unknown extensions", r["result"].get("version-rolling") is True and r["result"]["version-rolling.mask"] == "1ffe0000" and r["result"]["minimum-difficulty"] is False, str(r["result"]))
    mask = int(r["result"]["version-rolling.mask"], 16)
    c2 = await Client().open()
    r2 = await c2.request("mining.configure", [["version-rolling"], {"version-rolling.mask": "e0000000", "version-rolling.min-bit-count": 2}])
    h.check("configure refuses a mask without usable bits", r2["result"].get("version-rolling") is False)
    c2.w.close()

    r = await c.request("mining.subscribe", ["test-miner/1.0"])
    h.check("subscribe advertises empty extranonce1 and extranonce2 size 0", r["result"][1] == "" and r["result"][2] == 0, str(r["result"]))
    r = await c.request("mining.authorize", ["notanaddress.rig1", "x"])
    h.check("authorize rejects a non-address account", r["result"] is False and r["error"][0] == 24)
    r = await c.request("mining.authorize", [MINER_ADDR + ".rig1", "x"])
    h.check("authorize accepts a regtest address account", r["result"] is True)
    job = await c.wait_job()
    h.check("first notify arrives with set_difficulty before it", job is not None and c.diff == 0.001, f"diff {c.diff}")
    h.check("first job is a clean job", job[8] is True)

    height0 = n.rpc("getblockcount")
    rv = int(job[5], 16) | 0x00400000
    low_nonce, _ = grind(job, rv, want_block=False)
    rid = c.send("mining.submit", [MINER_ADDR + ".rig1", job[0], "", job[7], f"{low_nonce:08x}", f"{rv & mask:08x}"])
    m = await c.read()
    while m.get("id") != rid: m = await c.read()
    h.check("a hash above the share target is refused as low difficulty", m["error"] and m["error"][0] == 23, str(m["error"]))

    nonce, bhash = grind(job, rv)
    rid = c.send("mining.submit", [MINER_ADDR + ".rig1", job[0], "", job[7], f"{nonce:08x}", f"{rv & mask:08x}"])
    t0 = time.perf_counter()
    m = await c.read()
    while m.get("id") != rid: m = await c.read()
    h.check("a rolled-version share that meets the network target is accepted", m["result"] is True, str(m))
    for _ in range(100):
        if n.rpc("getblockcount") == height0 + 1: break
        await asyncio.sleep(0.05)
    h.check("node accepted the block built from the Stratum job", n.rpc("getblockcount") == height0 + 1 and n.rpc("getbestblockhash") == bhash, bhash[:16])
    paid = n.rpc("getblock", bhash, 2)["tx"][0]["vout"][0]["scriptPubKey"]["hex"]
    h.check("block pays one of the pool payout scripts", paid in scripts)

    new_job = await c.wait_job(clean=True, after=job[0])
    h.check("miner receives a clean job after the tip changed", new_job is not None and new_job[1] != job[1])
    rid = c.send("mining.submit", [MINER_ADDR + ".rig1", job[0], "", job[7], f"{nonce:08x}", f"{rv & mask:08x}"])
    m = await c.read()
    while m.get("id") != rid: m = await c.read()
    h.check("a share for the previous tip is stale", m["error"] and m["error"][0] == 21, str(m["error"]))

    rid = c.send("mining.submit", [MINER_ADDR + ".rig1", "deadbeef", "", new_job[7], "00000001", "00000000"])
    m = await c.read()
    while m.get("id") != rid: m = await c.read()
    h.check("an unknown job id is refused", m["error"] and m["error"][0] == 21)

    anon = await Client().open()
    await anon.request("mining.subscribe", ["x"])
    r = await anon.request("mining.submit", ["w", "1", "", "00000000", "00000000"])
    h.check("submit before authorize is refused", r["error"] and r["error"][0] == 24)
    anon.w.write(b"A" * 20000); await anon.w.drain()
    closed = False
    try:
        for _ in range(5):
            if not await asyncio.wait_for(anon.r.read(100), 2): closed = True; break
    except Exception: closed = True
    h.check("an oversized line closes the connection", closed)

    N = 60
    clients = [await Client().open() for _ in range(N)]
    for i, cl in enumerate(clients):
        await cl.request("mining.configure", [["version-rolling"], {"version-rolling.mask": "ffffffff"}])
        await cl.request("mining.subscribe", ["bulk"])
        await cl.request("mining.authorize", [f"{MINER_ADDR}.bulk{i}", "x"])
    jobs = [await cl.wait_job() for cl in clients]
    keys = {(j[3], j[5], j[7], j[2]) for j in jobs}
    h.check(f"{N} connections got {N} distinct (candidate, version, ntime) units", len(keys) == N, f"{len(keys)} distinct")
    api_miners = json.loads(get("/api/v1/miners?limit=1000"))
    h.check("admin API lists all sessions", len(api_miners) >= N + 1, str(len(api_miners)))

    old = [cl.job[0] for cl in clients]
    t0 = time.perf_counter()
    n.rpc("generatetodescriptor", 1, "raw(51)")
    got = await asyncio.gather(*[cl.wait_job(clean=True, after=o, timeout=5) for cl, o in zip(clients, old)])
    times = sorted((cl.job_time - t0) * 1000 for cl, j in zip(clients, got) if j)
    h.check(f"all {N} connections received a clean job after the new tip", len(times) == N, f"{len(times)}")
    if times: print(f"  info tip to clean job on the wire: p50 {times[len(times)//2]:.0f} ms, max {times[-1]:.0f} ms (includes node block generation and candidate rebuild)")

    await asyncio.sleep(1.5)
    summ = json.loads(get("/api/v1/summary"))
    h.check("summary counts miners and the accepted share", summ["miners"]["total"] >= N and summ["shares"]["total"] >= 3, f"{summ['miners']} {summ['shares']}")

with Harness("step09") as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
