import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, POOLPORT = 18981, 18333
def addr(label): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(label).digest()[:20]))
A, B, C = addr(b"acct-A"), addr(b"acct-B"), addr(b"acct-C")

def write_cfg(path, dry_run):
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 20

[pool]
network = "regtest"
refresh_secs = 0
spot_check = 2

[payout]
default_mode = "pplns"
wallet = "photon"
wallet_addresses = 4
dry_run = {str(dry_run).lower()}
interval_secs = 2
maturity_poll_secs = 1
min_payout = 0.0001

[payout.pplns]
window_factor = 1.0e9

[payout.pps]
reserve_min = 0.0

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

async def wait_for(cond, secs=30, step=0.2):
    end = time.time() + secs
    while time.time() < end:
        try:
            r = cond()
            if r: return r
        except Exception: pass
        await asyncio.sleep(step)
    return None

async def find_block(h, account, password="x"):
    c, r = await setup_miner(POOLPORT, f"{account}.rig", password)
    if r["result"] is not True: return None, r
    job = await c.wait_job()
    height0 = h.node.rpc("getblockcount")
    m, bhash = await mine_and_submit(c, f"{account}.rig", job)
    ok = m["result"] is True and await wait_for(lambda: h.node.rpc("getblockcount") == height0 + 1, 10)
    c.w.close()
    return (bhash if ok else None), m

async def phase1(h):
    n = h.node
    s = await wait_for(lambda: (lambda x: x if x["alloc"]["candidates"] == 4 else None)(get("/api/v1/summary")), 40)
    h.check("pool addresses were provisioned from the wallet (4 candidates)", s is not None)
    addrs = n.rpc("getaddressesbylabel", "photon-pool", wallet="photon")
    h.check("the four pool addresses belong to the node wallet", len(addrs) == 4)
    ppsr = await wait_for(lambda: get("/api/v1/ledger")["ppsReady"] is True, 15)
    h.check("PPS becomes available once the reserve condition is met (reserve 0 in this test)", ppsr)

    bA, mA = await find_block(h, A)
    h.check("account A finds a block in PPLNS mode", bA is not None, str(mA))
    await asyncio.sleep(0.5)
    bB, mB = await find_block(h, B)
    h.check("account B finds a block in PPLNS mode", bB is not None, str(mB))
    await asyncio.sleep(0.5)
    bC, mC = await find_block(h, C, "m=pps")
    h.check("account C finds a block in PPS mode", bC is not None, str(mC))
    blocks = get("/api/v1/blocks")
    h.check("ledger lists the three blocks with their modes", sorted(b["mode"] for b in blocks) == ["pplns", "pplns", "pps"], str([(b["height"], b["mode"]) for b in blocks]))
    accA = get(f"/api/v1/account/{A}")
    h.check("account view shows pending PPLNS credit for A before maturity", accA["balance"]["pending"] > 0 and accA["balance"]["confirmed"] == 0, str(accA["balance"]))
    rewards = [n.rpc("getblock", x, 2)["tx"][0]["vout"][0]["value"] for x in (bA, bB, bC)]
    R = int(Decimal(str(rewards[0])) * 100_000_000)
    n.rpc("generatetodescriptor", 101, "raw(51)")
    st = await wait_for(lambda: (lambda l: l if l["blocks"].get("mature") == 3 else None)(get("/api/v1/ledger")), 30)
    h.check("all three blocks reach maturity in the ledger", st is not None, str(get("/api/v1/ledger")["blocks"]))
    la = get(f"/api/v1/account/{A}")["balance"]; lb = get(f"/api/v1/account/{B}")["balance"]; lc = get(f"/api/v1/account/{C}")["balance"]
    net = lambda x: int(x * 0.99)
    h.check("A's confirmed balance is block 1 plus half of block 2 after the 1 % fee", abs(la["confirmed"] - (net(R) + net(R) // 2)) <= 2, f"{la['confirmed']} vs {net(R) + net(R) // 2}")
    h.check("B's confirmed balance is half of block 2 after the fee", abs(lb["confirmed"] - net(R) // 2) <= 2, f"{lb['confirmed']} vs {net(R) // 2}")
    expected_c = int(R * 0.98)
    h.check("C's PPS balance is one share, capped at the block reward minus the 2 % fee", abs(lc["confirmed"] - expected_c) <= 2, f"{lc['confirmed']} vs {expected_c}")
    await asyncio.sleep(5)
    led = get("/api/v1/ledger")
    h.check("dry run: balances are payable but nothing was sent", led["dryRun"] is True and led["payableAccounts"] == 3 and led["payouts"] == [], str(led))
    return R, (la["confirmed"], lb["confirmed"], lc["confirmed"])

async def phase2(h, R, expected):
    n = h.node
    led = await wait_for(lambda: (lambda l: l if l["payouts"] and l["payouts"][0]["txid"] else None)(get("/api/v1/ledger")), 40)
    h.check("after switching dry_run off the payout is broadcast", led is not None, str(get("/api/v1/ledger")))
    if not led: return
    p = led["payouts"][0]
    h.check("payout total equals the sum of the three balances", p["amount"] == sum(expected), f"{p['amount']} vs {sum(expected)}")
    tx = n.rpc("gettransaction", p["txid"], True, True, wallet="photon")
    outs = {}
    for o in tx["decoded"]["vout"]:
        a = o["scriptPubKey"].get("address")
        if a in (A, B, C): outs[a] = int(Decimal(str(o["value"])) * 100_000_000)
    h.check("the transaction pays each account its balance", outs == {A: expected[0], B: expected[1], C: expected[2]}, str(outs))
    h.check("liabilities are zero after the payout", get("/api/v1/ledger")["liabilities"] == 0)
    count = len(get("/api/v1/ledger")["payouts"])
    await asyncio.sleep(5)
    h.check("no second payout for the same balances", len(get("/api/v1/ledger")["payouts"]) == count)
    h.check("account A shows the payout and its paid total", get(f"/api/v1/account/{A}")["balance"]["paid"] == expected[0] and get(f"/api/v1/account/{A}")["payouts"][0]["txid"] == p["txid"])

with Harness("step11", extra=["-fallbackfee=0.0001"]) as h:
    base = h.node.datadir + "-cfg"
    cfg1, cfg2 = base + "1.toml", base + "2.toml"
    for ext in ("", "-wal", "-shm"):
        try: os.remove(cfg1 + ".db" + ext)
        except FileNotFoundError: pass
    write_cfg(cfg1, True)
    write_cfg(cfg2, False)
    open(cfg2, "a").write("")
    # second config must share the ledger database with the first
    s2 = open(cfg2).read().replace(f'{cfg2}.db', f'{cfg1}.db'); open(cfg2, "w").write(s2)
    d = subprocess.Popen([PHOTOND, "-c", cfg1, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        R, expected = asyncio.run(phase1(h))
    finally:
        d.terminate(); d.wait(timeout=10)
    d = subprocess.Popen([PHOTOND, "-c", cfg2, "run"], stdout=open(base + "-photond2.log", "w"), stderr=subprocess.STDOUT)
    try:
        asyncio.run(phase2(h, R, expected))
    finally:
        d.terminate(); d.wait(timeout=10)
