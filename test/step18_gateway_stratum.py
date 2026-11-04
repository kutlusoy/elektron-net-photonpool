"""Gateway Stratum V1 front end: an ASIC (simulated by the test client) mines on the gateway, the gateway forwards shares to the
pool over Photon, submits blocks to its own node, and the blocks pay the plan in the coinbase."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PHOTON, GWSTRATUM = 18981, 18340, 18444

def addr(i): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]))
def summary(): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())

def write_cfgs(base):
    open(base + ".toml", "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 50
[pool]
network = "regtest"
payout_addresses = {json.dumps([addr(1), addr(2)])}
refresh_secs = 0
spot_check = 2
[stratum]
enabled = false
[ledger]
path = "{base}.toml.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
[photon]
enabled = true
listen = "127.0.0.1:{PHOTON}"
key_file = "{base}.key"
fee_address = "{addr(900)}"
fee_pct = 1.0
initial_difficulty = 0.0001
min_difficulty = 0.00001
''')

def gateway_cfg(path, key):
    open(path, "w").write(f'''node_rpc_url = "http://127.0.0.1:{RPCPORT}"
node_auth = "t:t"
pool_addr = "127.0.0.1:{PHOTON}"
pool_key = "{key}"
network = "regtest"
payout_address = "{addr(5)}"
fee_address = "{addr(900)}"
fee_pct = 1.0
variants = 8
share_difficulty = 0.0001
[stratum]
listen = "127.0.0.1:{GWSTRATUM}"
initial_difficulty = 0.00001
min_job_interval_ms = 200
''')

def cb_outputs(n, height):
    cb = n.rpc("getblock", n.rpc("getblockhash", height), 2)["tx"][0]
    return {o["scriptPubKey"]["hex"]: int(Decimal(str(o["value"])) * LEPTON) for o in cb["vout"] if o["value"] > 0}, [o["scriptPubKey"]["hex"] for o in cb["vout"]]

async def asic(h, n):
    c, r = await setup_miner(GWSTRATUM, "asic-farm.rack1-unit7")
    h.check("the ASIC is authorized and negotiates version rolling", r.get("result") is True and c.mask)
    sub = None
    job = await c.wait_job(timeout=10)
    h.check("the gateway sends a job", job is not None)
    if not job: return
    # subscribe reply: empty extranonce1, extranonce2_size 0
    r = await c.request("mining.subscribe", ["again"])
    h.check("extranonce1 is empty and extranonce2_size is 0", r["result"][1] == "" and r["result"][2] == 0, str(r))
    cb = bytes.fromhex(job[2]) + bytes.fromhex(job[3])
    h.check("the job's coinbase carries the accounting commitment", (b"\x6a\x24" + b"PHOT") in cb)
    h.check("the job's coinbase pays the pool fee script", script_has(cb, p2wpkh_script(900)))

    start = n.rpc("getblockcount")
    for i in range(4):
        job = c.job
        m, bhash = await mine_and_submit(c, "asic-farm.rack1-unit7", job)
        h.check(f"block share {i + 1} is accepted by the gateway", m.get("result") is True, str(m))
        if i == 0:
            m2 = await submit_same(c, job)
            h.check("the same share again is refused as duplicate or stale", m2.get("error") is not None, str(m2))
        for _ in range(100):
            if n.rpc("getblockcount") == start + i + 1: break
            await asyncio.sleep(0.1)
        h.check(f"the gateway's node has block {i + 1} (hash {bhash[:12]})", n.rpc("getblockcount") == start + i + 1 and n.rpc("getbestblockhash") == bhash)
        # real blocks are minutes apart: let the pool follow the tip and the gateway announce before the next block
        for _ in range(50):
            if summary()["node"]["height"] >= start + i + 1: break
            await asyncio.sleep(0.1)
        await asyncio.sleep(0.4)
        nj = await c.wait_job(clean=True, timeout=10, after=job[0])
        h.check(f"a clean job for the new tip arrives after block {i + 1}", nj is not None)
        if not nj: return
    return start

def script_has(cb, script): return script in cb

async def submit_same(c, job):
    rv = int(job[5], 16) | 0x00400000
    nonce, _ = grind(job, rv)
    rid = c.send("mining.submit", ["asic-farm.rack1-unit7", job[0], "", job[7], f"{nonce:08x}", f"{rv & c.mask:08x}"])
    while True:
        m = await c.read()
        if m.get("id") == rid and "method" not in m: return m

with Harness("step18", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    base = n.datadir + "-cfg"
    for suffix in (".toml.db", ".toml.db-wal", ".toml.db-shm", ".key"):
        if os.path.exists(base + suffix): os.remove(base + suffix)
    write_cfgs(base)
    pool = subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool.log", "w"))
    gw = None
    try:
        for _ in range(200):
            try:
                if summary()["photon"].get("publicKey") and summary()["node"]["height"] > 0: break
            except Exception: pass
            time.sleep(0.2)
        key = summary()["photon"]["publicKey"]
        time.sleep(1.5)
        gateway_cfg(base + ".gw.toml", key)
        gw = subprocess.Popen([PHOTOND, "gateway", "--gateway-config", base + ".gw.toml"], stdout=subprocess.DEVNULL, stderr=open(base + ".gw.log", "w"))
        time.sleep(3)
        start = asyncio.run(asic(h, n))
        time.sleep(1.5)
        s = summary()["photon"]
        h.check("the pool received the forwarded shares and block reports", s["sharesOk"] >= 4 and s["blocks"] >= 4, str(s))
        h.check("no spot check failed", s["checksFailed"] == 0, str(s))
        if start is not None:
            last, scripts_ = cb_outputs(n, n.rpc("getblockcount"))
            total = sum(last.values())
            h.check("the latest block pays the ASIC's payout address most of the reward and the pool fee at least 1 %", last.get(p2wpkh_script(5).hex(), 0) >= int(total * 0.9) and last.get(p2wpkh_script(900).hex(), 0) >= total // 100, str(last))
            h.check("the block carries the accounting commitment", any(x.startswith("6a24" + b"PHOT".hex()) for x in scripts_))
    finally:
        if gw and gw.poll() is None: gw.terminate()
        pool.terminate(); pool.wait(timeout=20)
