import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

# Bitaxe (ESP-Miner) keeps up to 10 identical jobs queued ahead when the pool sends no extranonce2 space; it only drops them on a clean job.
# The pool therefore marks every fresh job clean for miners whose user agent is listed in [stratum] clean_work_agents, and only those.

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
initial_difficulty = 0.001
min_job_interval_ms = 200
[ledger]
path = "{path}.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

async def notifies(agent, want):
    c = await Client().open()
    await c.request("mining.configure", [["version-rolling"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}])
    await c.request("mining.subscribe", [agent])
    await c.request("mining.authorize", [ACCOUNT + ".rig1", "x"])
    out, end = [], time.perf_counter() + 6
    while len(out) < want and time.perf_counter() < end:
        try: m = await c.read(timeout=max(0.1, end - time.perf_counter()))
        except asyncio.TimeoutError: break
        if m.get("method") == "mining.notify": out.append((time.perf_counter(), m["params"][8]))
    c.w.close()
    return out

async def main(h):
    for _ in range(100):
        try:
            if json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=2).read())["alloc"]["candidates"] == NADDR: break
        except Exception: pass
        await asyncio.sleep(0.1)
    bit = await notifies("bitaxe/BM1370/v2.9.0", 3)
    h.check("a Bitaxe gets at least three jobs within a few seconds", len(bit) >= 3, str(len(bit)))
    h.check("every job for a Bitaxe is marked clean", bit and all(f is True for _, f in bit), str([f for _, f in bit]))
    other = await notifies("cgminer/4.11", 3)
    h.check("another miner gets the same number of jobs", len(other) >= 3, str(len(other)))
    h.check("its first job is clean and the following fresh jobs are not", other and other[0][1] is True and all(f is False for _, f in other[1:]), str([f for _, f in other]))

import urllib.request
with Harness("step24") as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
