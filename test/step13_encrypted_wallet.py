import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, POOLPORT = 18981, 18333
PASS = "correct horse battery staple"
A = encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"enc-acct-A").digest()[:20]))

def write_cfg(path):
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 20

[pool]
network = "regtest"
refresh_secs = 0

[payout]
wallet = "photon"
wallet_addresses = 4
dry_run = false
interval_secs = 2
maturity_poll_secs = 1
min_payout_leptons = 10000
wallet_passphrase_env = "PHOTON_WALLET_PASSPHRASE"

[payout.pplns]
window_factor = 1.0e9

[stratum]
listen = "127.0.0.1:{POOLPORT}"
initial_difficulty = 0.001

[ledger]
path = "{path}.db"

[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def get(path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=5).read())

async def wait_for(cond, secs=40, step=0.2):
    end = time.time() + secs
    while time.time() < end:
        try:
            r = cond()
            if r: return r
        except Exception: pass
        await asyncio.sleep(step)
    return None

async def main(h):
    n = h.node
    s = await wait_for(lambda: (lambda x: x if x["alloc"]["candidates"] == 4 else None)(get("/api/v1/summary")), 40)
    h.check("pool addresses are provisioned from the encrypted wallet", s is not None)
    info = n.rpc("getwalletinfo", wallet="photon")
    h.check("the wallet is locked while idle (unlocked_until is 0)", info.get("unlocked_until") == 0, str(info.get("unlocked_until")))

    c, r = await setup_miner(POOLPORT, f"{A}.rig")
    job = await c.wait_job()
    height0 = n.rpc("getblockcount")
    m, bhash = await mine_and_submit(c, f"{A}.rig", job)
    h.check("a PPLNS block is found", m["result"] is True)
    for _ in range(100):
        if n.rpc("getblockcount") == height0 + 1: break
        await asyncio.sleep(0.05)
    n.rpc("generatetodescriptor", 101, "raw(51)")
    led = await wait_for(lambda: (lambda l: l if l["payouts"] and l["payouts"][0]["txid"] else None)(get("/api/v1/ledger")), 40)
    h.check("the payout is sent from the encrypted wallet (unlocked with the passphrase from the environment)", led is not None, str(get("/api/v1/ledger"))[:300])
    time.sleep(1)
    h.check("the wallet is locked again after the payout", n.rpc("getwalletinfo", wallet="photon").get("unlocked_until") == 0)
    if led:
        tx = n.rpc("gettransaction", led["payouts"][0]["txid"], True, True, wallet="photon")
        paid = {o["scriptPubKey"].get("address"): Decimal(str(o["value"])) for o in tx["decoded"]["vout"]}
        h.check("the miner address received the PPLNS share", A in paid and paid[A] > 0, str(paid)[:200])

with Harness("step13", blocks=130, extra=["-fallbackfee=0.0001", "-wallet=photon"]) as h:
    n = h.node
    loaded = n.rpc("listwallets")
    h.check("a node started with -wallet=<name> for a wallet that does not exist yet comes up", True, f"wallets: {loaded}")
    if "photon" not in loaded:
        n.rpc("createwallet", "photon")
    n.rpc("encryptwallet", PASS, wallet="photon")
    time.sleep(1)
    for _ in range(40):
        try:
            n.rpc("getblockcount"); break
        except Exception: time.sleep(0.5)
    if "photon" not in n.rpc("listwallets"): n.rpc("loadwallet", "photon")
    h.check("encryptwallet works and keeps the node running", "unlocked_until" in n.rpc("getwalletinfo", wallet="photon"))
    n.rpc("walletpassphrase", PASS, 60, wallet="photon")
    d = n.rpc("listdescriptors", True, wallet="photon")
    h.check("the private-descriptor backup (listdescriptors true) works on the unlocked wallet", any("prv" in x["desc"] for x in d["descriptors"]), f"{len(d['descriptors'])} descriptors")
    n.rpc("walletlock", wallet="photon")
    cfg = h.node.datadir + "-cfg.toml"
    for ext in ("", "-wal", "-shm"):
        try: os.remove(cfg + ".db" + ext)
        except FileNotFoundError: pass
    write_cfg(cfg)
    env = dict(os.environ, PHOTON_WALLET_PASSPHRASE=PASS)
    p = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=open(h.node.datadir + "-photond.log", "w"), stderr=subprocess.STDOUT, env=env)
    try:
        asyncio.run(main(h))
    finally:
        p.terminate(); p.wait(timeout=10)
