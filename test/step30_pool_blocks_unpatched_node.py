import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

# End to end: blocks mined through the real pool (photond with the batch backend against a PATCHED node, Stratum miners in pool mode and in solo mode) are
# accepted by a second, UNPATCHED node. The node patch must never be needed to validate what the pool produces.
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
UNPATCHED = os.environ.get("UNPATCHED", "elektrond")
API, POOLPORT, SOLOPORT = 18981, 18333, 18334
def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]
def addr(label): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(label).digest()[:20]))

def write_cfg(path):
    addrs = [encode_segwit_address("bcrt", 0, list(prog(i))) for i in range(1, 9)]
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 20
[pool]
network = "regtest"
payout_addresses = {json.dumps(addrs)}
backend = "batch"
refresh_secs = 0
spot_check = 2
[payout]
default_mode = "pplns"
[stratum]
initial_difficulty = 0.001
min_job_interval_ms = 200
[[stratum.ports]]
listen = "127.0.0.1:{POOLPORT}"
mode = "pplns"
allow_override = true
[[stratum.ports]]
listen = "127.0.0.1:{SOLOPORT}"
mode = "solo"
allow_override = false
[ledger]
path = "{path}.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def get(path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=5).read())

async def main(h):
    a = h.node
    for _ in range(200):
        try:
            s = get("/api/v1/summary")
            if s["alloc"]["candidates"] == 8: break
        except Exception: pass
        await asyncio.sleep(0.1)
    h.check("the pool runs with the batch backend on the patched node", s["node"]["backend"] in ("batch", "getattestationbatch") or "batch" in str(s["node"]["backend"]), str(s["node"]["backend"]))
    solo, pplns = addr(b"e2e-solo"), addr(b"e2e-pplns")
    cs, r1 = await setup_miner(SOLOPORT, f"{solo}.rig1")
    cp, r2 = await setup_miner(POOLPORT, f"{pplns}.rig1")
    h.check("both miners are authorized", r1["result"] is True and r2["result"] is True)
    plan = [("pplns", cp, f"{pplns}.rig1"), ("solo", cs, f"{solo}.rig1"), ("pplns", cp, f"{pplns}.rig1"), ("solo", cs, f"{solo}.rig1"), ("pplns", cp, f"{pplns}.rig1"), ("pplns", cp, f"{pplns}.rig1")]
    pool_blocks, kinds = [], {}
    cur = {id(cs): await cs.wait_job(), id(cp): await cp.wait_job()}     # the jobs of the current tip
    for kind, c, worker in plan:
        job = cur[id(c)]
        h0 = a.rpc("getblockcount")
        m, bhash = await mine_and_submit(c, worker, job)
        for _ in range(200):
            if a.rpc("getblockcount") == h0 + 1: break
            await asyncio.sleep(0.05)
        ok = m["result"] is True and a.rpc("getblockcount") == h0 + 1
        h.check(f"{kind} block {h0 + 1} found through the pool and accepted by the patched node", ok, str(m))
        if ok: pool_blocks.append(h0 + 1); kinds[h0 + 1] = kind
        # every miner gets a clean job of the new tip: wait for it (a new job id) before the next round
        for cl in (cs, cp):
            nj = await cl.wait_job(clean=True, after=cur[id(cl)][0], timeout=10)
            if nj is None: h.check("a fresh job arrives after the block", False); return
            cur[id(cl)] = nj
    b = Node(UNPATCHED, os.environ.get("PHOTON_DATADIR", "/tmp/photon-test") + "-step30-b", ["-acceptnonstdtxn=1"], rpcport=18995, port=18996)
    b.start()
    try:
        h.check("the second node has no getattestationbatch (unpatched)", "getattestationbatch" not in b.rpc("help"))
        rej = []
        for height in range(1, a.rpc("getblockcount") + 1):
            r = b.rpc("submitblock", a.rpc("getblock", a.rpc("getblockhash", height), 0))
            if r is not None: rej.append((height, r))
        h.check("the unpatched node accepts every block of the chain, including all pool-mined ones", not rej and b.rpc("getbestblockhash") == a.rpc("getbestblockhash"), str(rej))
        h.check(f"{len(pool_blocks)} pool-mined blocks were in that chain", len(pool_blocks) == len(plan) and pool_blocks == list(range(131, 131 + len(plan))), str(pool_blocks))
        # the pool block of a solo miner pays that miner, a pool block pays a pool script: checked on the unpatched node's own copy
        solo_height = next(k for k, v in kinds.items() if v == "solo")
        solo_blk = b.rpc("getblock", b.rpc("getblockhash", solo_height), 2)
        h.check("the solo block pays the miner directly (seen by the unpatched node)", solo_blk["tx"][0]["vout"][0]["scriptPubKey"]["address"] == solo, str(solo_blk["tx"][0]["vout"][0]["scriptPubKey"].get("address")))
    finally:
        b.stop()

with Harness("step30", extra=["-acceptnonstdtxn=1"]) as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
