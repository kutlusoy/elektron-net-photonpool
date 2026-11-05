import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, STRATUM = 18981, 18333
NADDR = 4
ACCOUNT = encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"miner-account").digest()[:20]))

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
[stratum]
listen = "127.0.0.1:{STRATUM}"
initial_difficulty = 1024
min_job_interval_ms = 200
[ledger]
path = "{path}.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

# A slow miner (CPU) never finds a share at difficulty 1024: vardiff must lower the difficulty without any share, and the miner counts as online meanwhile.
async def main(h):
    for _ in range(100):
        try:
            if json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=2).read())["alloc"]["candidates"] == NADDR: break
        except Exception: pass
        await asyncio.sleep(0.1)
    c = await Client().open()
    await c.request("mining.subscribe", ["cpuminer/2.5.1"])
    await c.request("mining.authorize", [ACCOUNT + ".cpu", "x"])
    end, first = time.perf_counter() + 50, None
    while time.perf_counter() < end:
        try: await c.read(timeout=1)
        except asyncio.TimeoutError: pass
        if first is None and c.diff is not None: first = c.diff
        if c.diff is not None and c.diff < 1024: break
    h.check("the connection starts at difficulty 1024", first == 1024, str(first))
    h.check("vardiff lowers the difficulty although no share was submitted", c.diff is not None and c.diff < 1024, str(c.diff))
    await asyncio.sleep(2)
    s = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=2).read())
    h.check("a connected miner without shares counts as online", s["miners"]["online"] == 1, str(s["miners"]))

import urllib.request
with Harness("step27") as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
