import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, POOLPORT, SOLOPORT = 18981, 18333, 18334

def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]
def addr(label): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(label).digest()[:20]))
def script_hex(a): return (b"\x00\x14" + hashlib.sha256(a).digest()[:20]).hex()

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
spot_check = 2

[payout]
default_mode = "pplns"
max_solo_accounts = 3

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

[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def get(path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=5).read())

async def main(h):
    n = h.node
    pool_scripts = {(b"\x00\x14" + prog(i)).hex() for i in range(1, 5)}
    for _ in range(200):
        try:
            if get("/api/v1/summary")["alloc"]["candidates"] == 4: break
        except Exception: pass
        await asyncio.sleep(0.1)

    s1, s2, s3, s4 = (addr(b"solo-%d" % i) for i in range(1, 5))
    payout_of = lambda a: script_hex(b"solo-%d" % (int(a[-1:], 36) if False else 0))
    sc = {a: (b"\x00\x14" + hashlib.sha256(b"solo-%d" % i).digest()[:20]).hex() for i, a in enumerate((s1, s2, s3, s4), 1)}

    c1, r = await setup_miner(SOLOPORT, f"{s1}.rig1")
    h.check("solo port: authorize with an address succeeds", r["result"] is True)
    j1 = await c1.wait_job()
    h.check("solo port: a job arrives", j1 is not None)
    h.check("solo job's coinbase pays the miner address directly", sc[s1][:4] == "0014" and ("0014" + sc[s1][4:]) in j1[3], j1[3][:60])
    h.check("solo job's coinbase does not pay a pool address", not any(p in j1[3] for p in pool_scripts))

    ca, r = await setup_miner(POOLPORT, f"{s2}.rigA")
    ja = await ca.wait_job()
    h.check("pool port default mode pplns: coinbase pays a pool address", any(p in ja[3] for p in pool_scripts) and sc[s2] not in ja[3])

    cb, r = await setup_miner(POOLPORT, f"{s2}.rigB", "x,m=solo")
    h.check("password m=solo on a port with override works", r["result"] is True)
    jb = await cb.wait_job()
    h.check("override to solo gives a direct payout coinbase", sc[s2] in jb[3])

    cs, r = await setup_miner(SOLOPORT, f"{s3}.rig1", "m=pplns")
    jc = await cs.wait_job()
    h.check("password override is ignored on a fixed solo port", sc[s3] in jc[3] and not any(p in jc[3] for p in pool_scripts))

    cp, r = await setup_miner(POOLPORT, f"{s2}.rigP", "m=pps")
    h.check("PPS is refused while the reserve is not funded", r["result"] is False and r["error"] and r["error"][0] == 20 and "PPS" in r["error"][1], str(r["error"]))

    c1b, r = await setup_miner(SOLOPORT, f"{s1}.rig2")
    j1b = await c1b.wait_job()
    h.check("two workers of one solo account share the candidate but get different units", j1b[3] == j1[3] and (j1b[5], j1b[7]) != (j1[5], j1[7]), f"{(j1[5], j1[7])} vs {(j1b[5], j1b[7])}")

    c4, r = await setup_miner(SOLOPORT, f"{s4}.rig1")
    j4 = await c4.wait_job(timeout=2.5)
    h.check("a fourth solo account is not served beyond max_solo_accounts", j4 is None)
    c4.w.close()

    height0 = n.rpc("getblockcount")
    m, bhash = await mine_and_submit(c1, f"{s1}.rig1", j1)
    h.check("solo share meeting the network target is accepted", m["result"] is True, str(m))
    for _ in range(100):
        if n.rpc("getblockcount") == height0 + 1: break
        await asyncio.sleep(0.05)
    blk = n.rpc("getblock", bhash, 2)
    h.check("node accepted the solo block, reward output goes to the solo miner address", n.rpc("getblockcount") == height0 + 1 and blk["tx"][0]["vout"][0]["scriptPubKey"]["hex"] == sc[s1])

    blocks = []
    for _ in range(60):
        blocks = get("/api/v1/blocks")
        if blocks: break
        await asyncio.sleep(0.05)
    h.check("blocks API reports the finder's mode", blocks and blocks[0]["mode"] == "solo", str(blocks[:1]))

    nj = await c1.wait_job(clean=True, after=j1[0])
    h.check("solo miner gets a clean job on the new tip with its own payout script", nj is not None and sc[s1] in nj[3])
    nja = await ca.wait_job(clean=True, after=ja[0])
    h.check("pool miner gets a clean job on the new tip with a pool payout script", nja is not None and any(p in nja[3] for p in pool_scripts))

    height1 = n.rpc("getblockcount")
    m, bhash2 = await mine_and_submit(ca, f"{s2}.rigA", nja)
    h.check("pool-mode share meeting the network target is accepted", m["result"] is True, str(m))
    for _ in range(100):
        if n.rpc("getblockcount") == height1 + 1: break
        await asyncio.sleep(0.05)
    paid = n.rpc("getblock", bhash2, 2)["tx"][0]["vout"][0]["scriptPubKey"]["hex"]
    h.check("pool-mode block pays a pool address, not the miner", paid in pool_scripts)

with Harness("step10") as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
