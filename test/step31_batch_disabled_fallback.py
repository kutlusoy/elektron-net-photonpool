import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
os.environ.pop("ELEKTROND_BATCH_OPTION", None)   # this test runs a node of the photonpool branch WITHOUT -enableattestationbatch
import asyncio, json, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

# A node that has getattestationbatch but with the option off answers "disabled". The pool in backend "auto" must treat that like a missing RPC and fall back
# to one getblocktemplate call per payout address, and still serve work and find blocks.
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
backend = "auto"
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

async def main(h):
    n = h.node
    try: n.rpc("getattestationbatch", {"candidates": [{"outputs": [{"scriptPubKey": "51"}]}]}); disabled = False
    except RuntimeError as e: disabled = "disabled" in str(e)
    h.check("the node answers that getattestationbatch is disabled", disabled)
    s = None
    for _ in range(200):
        try:
            s = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=2).read())
            if s["alloc"]["candidates"] == 4: break
        except Exception: pass
        await asyncio.sleep(0.1)
    h.check("the pool falls back to getblocktemplate and has all candidates", s is not None and s["alloc"]["candidates"] == 4 and s["node"]["backend"] == "gbt", str(s and s["node"]["backend"]))
    c, r = await setup_miner(STRATUM, f"{ACCOUNT}.rig1")
    job = await c.wait_job()
    h0 = n.rpc("getblockcount")
    m, _ = await mine_and_submit(c, f"{ACCOUNT}.rig1", job)
    for _ in range(100):
        if n.rpc("getblockcount") == h0 + 1: break
        await asyncio.sleep(0.05)
    h.check("a block is found and accepted through the fallback", m["result"] is True and n.rpc("getblockcount") == h0 + 1, str(m))

with Harness("step31", extra=["-acceptnonstdtxn=1"]) as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
