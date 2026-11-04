"""One gateway, several payout addresses: workers named <address>.<worker> are credited to their own channel, the coinbase pays
every channel according to the shares, each owner can verify its own credit, and the pool's channel limit is respected."""
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
max_channels_per_connection = 3
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
    return {o["scriptPubKey"]["hex"]: int(Decimal(str(o["value"])) * LEPTON) for o in cb["vout"] if o["value"] > 0}

async def drain(c):
    while True:
        try: await c.read(timeout=0.4)
        except asyncio.TimeoutError: return

async def main(h, n, base):
    A, B = addr(10), addr(11)
    ca, ra = await setup_miner(GWSTRATUM, f"{A}.rig1")
    cb, rb = await setup_miner(GWSTRATUM, f"{B}.rig1")
    cd, rd = await setup_miner(GWSTRATUM, "not-an-address.rig9")
    h.check("workers for two different payout addresses are authorized", ra["result"] is True and rb["result"] is True, f"{ra} {rb}")
    h.check("a user without an address mines for the gateway's default channel", rd["result"] is True, str(rd))
    cx, rx = await setup_miner(GWSTRATUM, f"{addr(12)}.rig1")
    h.check("a fourth channel is refused because the pool limits channels per connection", rx["result"] is False and rx["error"] and "limit" in json.dumps(rx["error"]), str(rx))
    await asyncio.sleep(1.0)
    start = n.rpc("getblockcount")
    for i in range(8):
        c = (ca, cb)[i % 2]
        await drain(c)
        job = c.job
        m, bhash = await mine_and_submit(c, "w", job)
        h.check(f"block {i + 1} found by the {'A' if i % 2 == 0 else 'B'} worker is accepted", m.get("result") is True, str(m))
        for _ in range(100):
            if n.rpc("getblockcount") == start + i + 1: break
            await asyncio.sleep(0.1)
        for _ in range(50):
            if summary()["node"]["height"] >= start + i + 1: break
            await asyncio.sleep(0.1)
        await asyncio.sleep(0.5)
    return start

with Harness("step19", extra=["-acceptnonstdtxn=1"]) as h:
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
        start = asyncio.run(main(h, n, base))
        time.sleep(1.5)
        s = summary()["photon"]
        h.check("the pool has the default channel and the two worker channels", s["channels"] == 3, str(s))
        h.check("shares of all channels were accepted and no spot check failed", s["sharesOk"] >= 8 and s["checksFailed"] == 0, str(s))
        outs = cb_outputs(n, n.rpc("getblockcount"))
        total = sum(outs.values())
        pa, pb = outs.get(p2wpkh_script(10).hex(), 0), outs.get(p2wpkh_script(11).hex(), 0)
        h.check("the latest coinbase pays both worker addresses about half of the reward each", pa >= total * 0.35 and pb >= total * 0.35 and abs(pa - pb) <= total * 0.1, str(outs))
        h.check("the pool fee output holds at least 1 %", outs.get(p2wpkh_script(900).hex(), 0) >= total // 100, str(outs))
        log = open(base + ".gw.log").read()
        h.check("each channel owner's credit is verified against the committed root", all(p2wpkh_script(i).hex() in log and "verified against the committed root" in log for i in (10, 11)), log[-600:])
    finally:
        if gw and gw.poll() is None: gw.terminate()
        pool.terminate(); pool.wait(timeout=20)
