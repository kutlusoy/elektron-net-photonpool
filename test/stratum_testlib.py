import asyncio, json, struct, time
from photon_testlib import sha256d

STRATUM = 18333

def swap_words(h):
    b = bytes.fromhex(h)
    return b"".join(b[i:i + 4][::-1] for i in range(0, 32, 4))

class Client:
    def __init__(self, port=None): self.port = port or STRATUM; self.id = 0; self.pending = []; self.job = None; self.diff = None; self.mask = None
    async def open(self):
        self.r, self.w = await asyncio.open_connection("127.0.0.1", self.port)
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


async def setup_miner(port, account_worker, password="x"):
    c = await Client(port).open()
    r = await c.request("mining.configure", [["version-rolling"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}])
    c.mask = int(r["result"]["version-rolling.mask"], 16)
    await c.request("mining.subscribe", ["test-miner/1.0"])
    r = await c.request("mining.authorize", [account_worker, password])
    return c, r

async def mine_and_submit(c, worker, job=None, roll=0x00400000):
    job = job or c.job
    rv = int(job[5], 16) | roll
    nonce, bhash = grind(job, rv)
    rid = c.send("mining.submit", [worker, job[0], "", job[7], f"{nonce:08x}", f"{rv & c.mask:08x}"])
    while True:
        m = await c.read()
        if m.get("id") == rid and "method" not in m: return m, bhash
