import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, sqlite3, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

# The pool starts a connection with what it knows about the kind of device: what it has learned from earlier connections (kept in the ledger), else a typical
# value for the chip named in the user agent, else the configured default. Here the ledger has a learned row for the BM1366 and none for the BM1370.
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, STRATUM = 18981, 18333
ACCOUNT = encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"miner-account").digest()[:20]))
def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]

def write_cfg(path):
    addrs = [encode_segwit_address("bcrt", 0, list(prog(i))) for i in range(1, 5)]
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
min_job_interval_ms = 25
[ledger]
path = "{path}.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def preload(db):
    c = sqlite3.connect(db)
    c.execute("CREATE TABLE IF NOT EXISTS device_learned(key TEXT PRIMARY KEY, hashrate REAL NOT NULL, job_ms REAL NOT NULL, clean INTEGER NOT NULL, samples INTEGER NOT NULL, updated INTEGER NOT NULL) WITHOUT ROWID")
    c.execute("INSERT OR REPLACE INTO device_learned VALUES ('bitaxe/bm1366', 2.0e12, 0, 0, 5, ?)", (int(time.time()),))
    c.execute("INSERT OR REPLACE INTO device_learned VALUES ('cpuminer', 3.0e6, 0, 0, 2, ?)", (int(time.time()),))   # too few observations: not used yet
    c.commit(); c.close()

async def first_difficulty(agent):
    c = await Client().open()
    await c.request("mining.configure", [["version-rolling"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}])
    await c.request("mining.subscribe", [agent])
    await c.request("mining.authorize", [ACCOUNT + ".rig", "x"])
    await c.wait_job()
    d = c.diff
    c.w.close()
    return d

async def main(h):
    for _ in range(100):
        try:
            if json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=2).read())["alloc"]["candidates"] == 4: break
        except Exception: pass
        await asyncio.sleep(0.1)
    d66 = await first_difficulty("bitaxe/BM1366/v2.9.0")
    d70 = await first_difficulty("bitaxe/BM1370/v2.9.0")
    dcpu = await first_difficulty("cpuminer/2.5.1")
    h.check("a device type with learned values starts at the difficulty of its learned rate (2 TH/s, one share per 10 s)", d66 is not None and abs(d66 - 2.0e12 * 10 / 4294967296) < 1, str(d66))
    h.check("a chip without learned values starts at the typical rate of the chip (1.2 TH/s)", d70 is not None and abs(d70 - 1.2e12 * 10 / 4294967296) < 1, str(d70))
    h.check("software without enough learned observations starts at the configured difficulty", dcpu == 1024, str(dcpu))
    learned = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/learned", timeout=2).read())
    h.check("the operator API lists what was learned", any(x["device"] == "bitaxe/bm1366" and x["samples"] == 5 for x in learned), str(learned))

with Harness("step32", extra=["-acceptnonstdtxn=1"]) as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    if os.path.exists(cfg + ".db"): os.remove(cfg + ".db")
    preload(cfg + ".db")
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
