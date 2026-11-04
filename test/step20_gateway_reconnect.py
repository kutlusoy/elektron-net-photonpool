"""The gateway survives a pool outage: ASICs keep mining on the plans it already holds, new addresses are refused while the pool is
down, the gateway reconnects with backoff, re-opens its channels and re-announces the template; fatal errors are not retried."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, signal, subprocess, time, urllib.request
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PHOTON, GWSTRATUM = 18981, 18340, 18444

def addr(i): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]))
def summary(): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())
def up():
    try: return summary()["photon"].get("publicKey") and summary()["node"]["height"] > 0
    except Exception: return False

def pool_cfg(base):
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

def gateway_cfg(path, key, extra=""):
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
max_reconnect_secs = 4
{extra}
[stratum]
listen = "127.0.0.1:{GWSTRATUM}"
initial_difficulty = 0.00001
min_job_interval_ms = 200
''')

def outs(n, height):
    cb = n.rpc("getblock", n.rpc("getblockhash", height), 2)["tx"][0]
    return {o["scriptPubKey"]["hex"]: int(Decimal(str(o["value"])) * LEPTON) for o in cb["vout"] if o["value"] > 0}

async def drain(c):
    while True:
        try: await c.read(timeout=0.4)
        except asyncio.TimeoutError: return

async def mine_one(h, c, n, label):
    await drain(c)
    before = n.rpc("getblockcount")
    m, bhash = await mine_and_submit(c, "w", c.job)
    for _ in range(100):
        if n.rpc("getblockcount") == before + 1: break
        await asyncio.sleep(0.1)
    h.check(f"{label}: the block is accepted by the gateway's node", m.get("result") is True and n.rpc("getblockcount") == before + 1, str(m))
    await asyncio.sleep(0.6)

async def scenario(h, n, base, pool_box):
    A = addr(10)
    c, r = await setup_miner(GWSTRATUM, f"{A}.rig1")
    h.check("the worker is authorized", r["result"] is True)
    await asyncio.sleep(1.0)
    for i in range(3): await mine_one(h, c, n, f"before the outage, block {i + 1}")
    await asyncio.sleep(1.0)

    # --- outage ---
    pool_box[0].send_signal(signal.SIGINT); pool_box[0].wait(timeout=30)
    t_out = time.time()
    await asyncio.sleep(1.5)
    log = open(base + ".gw.log").read()
    h.check("the gateway notices the lost connection and reconnects with backoff", "reconnecting in" in log, log[-300:])
    c2, r2 = await setup_miner(GWSTRATUM, f"{A}.rig2")
    h.check("a worker of a known address can still connect during the outage", r2["result"] is True, str(r2))
    c3, r3 = await setup_miner(GWSTRATUM, f"{addr(77)}.rig1")
    h.check("a new address is refused while the pool is down", r3["result"] is False and "down" in json.dumps(r3["error"]), str(r3))
    await mine_one(h, c, n, "during the outage")
    ob = outs(n, n.rpc("getblockcount"))
    h.check("the outage block still pays the last known plan (worker A and the pool fee)", ob.get(p2wpkh_script(10).hex(), 0) > 0 and ob.get(p2wpkh_script(900).hex(), 0) > 0, str(ob))

    # --- pool is back ---
    pool_box[0] = subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool2.log", "w"))
    for _ in range(300):
        if up(): break
        await asyncio.sleep(0.2)
    for _ in range(300):
        try:
            if summary()["photon"]["connections"] >= 1 and summary()["photon"]["channels"] >= 2: break
        except Exception: pass
        await asyncio.sleep(0.2)
    s = summary()["photon"]
    h.check(f"the gateway is back after {time.time() - t_out:.0f} s with its channels re-opened (default and worker A)", s["connections"] >= 1 and s["channels"] >= 2, str(s))
    await asyncio.sleep(2.0)
    for i in range(2):
        await mine_one(h, c, n, f"after the reconnect, block {i + 1}")
        for _ in range(50):
            if summary()["node"]["height"] >= n.rpc("getblockcount"): break
            await asyncio.sleep(0.1)
    await asyncio.sleep(1.0)
    s = summary()["photon"]
    h.check("shares of the new connection are credited and no spot check failed", s["sharesOk"] >= 2 and s["checksFailed"] == 0, str(s))
    last = outs(n, n.rpc("getblockcount"))
    total = sum(last.values())
    h.check("the credit of before the outage is remembered: the latest coinbase pays worker A almost the whole reward", last.get(p2wpkh_script(10).hex(), 0) >= int(total * 0.9) and last.get(p2wpkh_script(900).hex(), 0) >= total // 100, str(last))
    log = open(base + ".gw.log").read()
    h.check("the gateway verified A's credit again on the new connection", log.count("verified against the committed root") >= 2 and p2wpkh_script(10).hex() in log)

with Harness("step20", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    base = n.datadir + "-cfg"
    for suffix in (".toml.db", ".toml.db-wal", ".toml.db-shm", ".key"):
        if os.path.exists(base + suffix): os.remove(base + suffix)
    pool_cfg(base)
    pool_box = [subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool.log", "w"))]
    gw = None
    try:
        for _ in range(200):
            if up(): break
            time.sleep(0.2)
        key = summary()["photon"]["publicKey"]
        time.sleep(1.5)
        gateway_cfg(base + ".gw.toml", key)
        gw = subprocess.Popen([PHOTOND, "gateway", "--gateway-config", base + ".gw.toml"], stdout=subprocess.DEVNULL, stderr=open(base + ".gw.log", "w"))
        time.sleep(3)
        asyncio.run(scenario(h, n, base, pool_box))
        h.check("the gateway process is still running", gw.poll() is None)
        gw.terminate(); gw.wait(timeout=20); gw = None

        # a wrong pinned key is a configuration error: the gateway stops instead of retrying
        gateway_cfg(base + ".gw2.toml", "00" * 32)
        t0 = time.time()
        bad = subprocess.run([PHOTOND, "gateway", "--gateway-config", base + ".gw2.toml"], capture_output=True, text=True, timeout=30)
        h.check("a wrong pool key ends the gateway immediately", bad.returncode != 0 and "pinned key" in bad.stderr and time.time() - t0 < 15, bad.stderr[-300:])
        # an unreachable pool is retried; with reconnect = false it is an error
        gateway_cfg(base + ".gw3.toml", key, "reconnect = false")
        pool_box[0].send_signal(signal.SIGINT); pool_box[0].wait(timeout=30)
        off = subprocess.run([PHOTOND, "gateway", "--gateway-config", base + ".gw3.toml"], capture_output=True, text=True, timeout=30)
        h.check("with reconnect = false an unreachable pool is an error", off.returncode != 0)
    finally:
        if gw and gw.poll() is None: gw.terminate()
        if pool_box[0].poll() is None: pool_box[0].terminate(); pool_box[0].wait(timeout=20)
