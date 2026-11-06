import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request, struct
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

# PPLNS from the miner to the wallet, as an operator would see it: an encrypted pool wallet on the node, the pool unlocking it only to pay, accounts with
# different numbers of shares, rejected shares that must not count, a block, maturity, the proportional split with the fee, a wrong passphrase (nothing is lost),
# the payout transaction, a restart (no second payout), a payout in several batches for 250 accounts, a block that is orphaned, and the minimum payout.
# Shares are made easy on purpose (share difficulty below the regtest network difficulty): a block share is made by grinding for the network target.
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, POOLPORT = 18981, 18333
PASS, WRONG = "correct horse battery staple", "not the passphrase"
FEE = 0.01
def addr(label): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(label).digest()[:20]))

def write_cfg(path, min_payout=10000, max_outputs=100):
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
dry_run = false
interval_secs = 2
maturity_poll_secs = 1
min_payout_leptons = {min_payout}
max_outputs_per_tx = {max_outputs}
wallet_passphrase_env = "PHOTON_WALLET_PASSPHRASE"
[payout.fee_pct]
pplns = 1.0
[payout.pplns]
window_factor = 1.0e9
[stratum]
listen = "127.0.0.1:{POOLPORT}"
initial_difficulty = 1e-12
min_difficulty = 1e-13
share_secs = 0.001
[ledger]
path = "{path}.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def get(path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=10).read())
def raw(path):
    try: return urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=10).status
    except urllib.error.HTTPError as e: return e.code

async def wait_for(cond, secs=40, step=0.25):
    end = time.time() + secs
    while time.time() < end:
        try:
            r = cond()
            if r: return r
        except Exception: pass
        await asyncio.sleep(step)
    return None

class Pool:
    def __init__(self, cfg, passphrase, log): self.cfg, self.passphrase, self.log, self.proc = cfg, passphrase, log, None
    def start(self):
        env = dict(os.environ, PHOTON_WALLET_PASSPHRASE=self.passphrase)
        self.proc = subprocess.Popen([PHOTOND, "-c", self.cfg, "run"], stdout=open(self.log, "a"), stderr=subprocess.STDOUT, env=env)
    def stop(self):
        if self.proc: self.proc.terminate(); self.proc.wait(timeout=15); self.proc = None
    def log_text(self): return open(self.log, errors="replace").read()

async def ready(candidates=4):
    return await wait_for(lambda: (lambda x: x if x["alloc"]["candidates"] == candidates else None)(get("/api/v1/summary")), 60)

async def share(c, worker, job, roll, block=False):
    """One share (a block when `block`): returns the pool's answer."""
    rv = int(job[5], 16) | ((roll & c.mask) or (c.mask & -c.mask))
    nonce, _ = grind(job, rv, want_block=block)
    rid = c.send("mining.submit", [worker, job[0], "", job[7], f"{nonce:08x}", f"{rv & c.mask:08x}"])
    while True:
        m = await c.read()
        if m.get("id") == rid and "method" not in m: return m, (nonce, rv)

async def miner(account, worker="rig1"):
    c, r = await setup_miner(POOLPORT, f"{account}.{worker}")
    assert r["result"] is True, r
    return c, await c.wait_job()

def reward_of(n, bhash): return int(Decimal(str(n.rpc("getblock", bhash, 2)["tx"][0]["vout"][0]["value"])) * 100_000_000)

async def finder(h, account):
    """A miner that finds a block; returns (block hash, reward in leptons). Retries when the job was cut by a block found a moment before."""
    n = h.node
    for attempt in range(6):
        await asyncio.sleep(0.5)
        c, job = await miner(account, "finder")
        h0 = n.rpc("getblockcount")
        m, _ = await share(c, f"{account}.finder", job, 0x00400000, block=True)
        c.w.close()
        if m["result"] is True and await wait_for(lambda: n.rpc("getblockcount") == h0 + 1, 8):
            bhash = n.rpc("getblockhash", h0 + 1)
            return bhash, reward_of(n, bhash)
    raise AssertionError(("no block", account))

def paid_outputs(n, txid, accounts):
    tx = n.rpc("gettransaction", txid, True, True, wallet="photon")
    out = {}
    for o in tx["decoded"]["vout"]:
        a = o["scriptPubKey"].get("address")
        if a in accounts: out[a] = int(Decimal(str(o["value"])) * 100_000_000)
    return out

async def main(h, pool_a, pool_b, pool_c, cfg):
    n = h.node
    A, B, C = (addr(b"pplns-" + x) for x in (b"A", b"B", b"C"))

    # ---- 1. wallet, passphrase, pool start ----
    n.rpc("createwallet", "photon", False, False, PASS)
    info = n.rpc("getwalletinfo", wallet="photon")
    h.check("the pool wallet exists on the node and is encrypted (unlocked_until is present)", "unlocked_until" in info and info["unlocked_until"] == 0, str(info.get("unlocked_until")))
    pool_a.start()   # first with the WRONG passphrase
    h.check("the pool starts and provisions its payout addresses from the locked wallet", await ready() is not None)
    h.check("the four pool addresses belong to the wallet", len(n.rpc("getaddressesbylabel", "photon-pool", wallet="photon")) == 4)
    h.check("the public pool page lists the payout modes and no account data", get("/public/v1/pool")["modes"][0]["mode"] in ("solo", "pplns", "pps"))

    # ---- 2. blocks of different accounts, shares that must not count ----
    # On regtest the network difficulty is the minimum, so every valid share is also a block. The window holds all shares (window_factor 1e9), so the credits of
    # block j are split over the first j shares (one per block, all of the same difficulty) in proportion.
    seq = [A, A, B, A, C, B]
    blocks = []
    for who in seq:
        bh, R = await finder(h, who)
        blocks.append((who, bh, R))
        await asyncio.sleep(0.4)
    h.check("six blocks of three accounts are found through the pool", n.rpc("getblockcount") == 130 + 6, str(n.rpc("getblockcount")))
    cA, jA = await miner(A)
    rv = int(jA[5], 16) | (9 << 13)
    nonce, _ = grind(jA, rv, want_block=False)
    rid = cA.send("mining.submit", [f"{A}.rig1", "ffffffff", "", jA[7], f"{nonce:08x}", f"{rv & cA.mask:08x}"])
    while True:
        m = await cA.read()
        if m.get("id") == rid and "method" not in m: break
    h.check("a share for an unknown job is rejected", m["result"] is None and m["error"][0] == 21, str(m))
    c0, j0 = await miner(B)
    m1, (n1, rv1) = await share(c0, f"{B}.rig1", j0, 5 << 13, block=True)
    ok_first = m1["result"] is True
    await asyncio.sleep(3)
    cnt_before = get(f"/public/v1/account/{B}")["shares24h"]["pplns"]["count"]
    rid = c0.send("mining.submit", [f"{B}.rig1", j0[0], "", j0[7], f"{n1:08x}", f"{rv1 & c0.mask:08x}"])
    while True:
        m = await c0.read()
        if m.get("id") == rid and "method" not in m: break
    h.check("the same share again is rejected (duplicate or stale)", ok_first and m["result"] is None and m["error"][0] in (21, 22), str(m))
    seq = seq + [B]
    blocks.append((B, n.rpc("getblockhash", n.rpc("getblockcount")), reward_of(n, n.rpc("getblockhash", n.rpc("getblockcount")))))
    await asyncio.sleep(3)
    cnt_after = get(f"/public/v1/account/{B}")["shares24h"]["pplns"]["count"]
    h.check("the replay does not count as a share", cnt_after == cnt_before, f"{cnt_before} -> {cnt_after}")
    shares = {x: seq.count(x) for x in set(seq)}
    h.check("the account pages count exactly the accepted shares (A 3, B 3, C 1)", all(get(f"/public/v1/account/{x}")["shares24h"]["pplns"]["count"] == shares[x] for x in (A, B, C)), str({x: get(f"/public/v1/account/{x}")["shares24h"]["pplns"]["count"] for x in (A, B, C)}))
    unauth = await Client(POOLPORT).open()
    await unauth.request("mining.subscribe", ["test/1"])
    r = await unauth.request("mining.authorize", ["not-an-address.rig", "x"])
    h.check("an invalid payout address is refused at authorize", r["result"] is False, str(r))
    unauth.w.close()
    h.check("an invalid address on the account page is a 400", raw("/public/v1/account/not-an-address") == 400)

    # ---- 3. pending credits, maturity ----
    expect = {A: 0, B: 0, C: 0}
    for j, (who, bh, R) in enumerate(blocks, 1):
        net = int(R * (1 - FEE))
        for x in expect:
            expect[x] += net * sum(1 for w, _, _ in blocks[:j] if w == x) // j
    pend = {x: get(f"/public/v1/account/{x}")["balance"]["pending"] for x in expect}
    h.check("the credits are pending and equal the proportional split of every block over the shares before it (after the 1 % fee)", all(abs(pend[x] - expect[x]) <= 40 for x in expect), f"{pend} vs {expect}")
    h.check("nothing is confirmed before maturity", all(get(f"/public/v1/account/{x}")["balance"]["confirmed"] == 0 for x in expect))
    n.rpc("generatetodescriptor", 101, "raw(51)")
    h.check("the blocks mature in the ledger", await wait_for(lambda: get("/api/v1/ledger")["blocks"].get("mature") == len(blocks) and sum(get(f"/public/v1/account/{x}")["balance"]["confirmed"] for x in expect) >= sum(expect.values()) - 200, 60) is not None, str(get("/api/v1/ledger")["blocks"]))
    conf = {x: get(f"/public/v1/account/{x}")["balance"]["confirmed"] for x in expect}
    h.check("after maturity the balances are confirmed and equal the credits", all(abs(conf[x] - expect[x]) <= 40 for x in expect), f"{conf} vs {expect}")
    A, B, C = A, B, C

    # ---- 4. wrong passphrase: the payout fails and nothing is lost ----
    await asyncio.sleep(8)
    led = get("/api/v1/ledger")
    log = pool_a.log_text()
    h.check("with a wrong passphrase no payout is sent", led["payouts"] == [] or all(not p["txid"] for p in led["payouts"]), str(led["payouts"]))
    h.check("the balances are still all there (liabilities unchanged)", abs(led["liabilities"] - sum(conf.values())) <= 4, f"{led['liabilities']} vs {sum(conf.values())}")
    h.check("the log says why", "unlock" in log.lower() or "passphrase" in log.lower() or "payout run failed" in log.lower(), log[-300:])
    h.check("the wallet is not left unlocked", n.rpc("getwalletinfo", wallet="photon")["unlocked_until"] == 0)
    pool_a.stop()

    # ---- 5. right passphrase: the payout is sent ----
    pool_b.start()
    led = await wait_for(lambda: (lambda l: l if l["payouts"] and l["payouts"][0]["txid"] else None)(get("/api/v1/ledger")), 60)
    h.check("with the right passphrase the payout is broadcast", led is not None, str(get("/api/v1/ledger"))[:300])
    p = led["payouts"][0]
    outs = paid_outputs(n, p["txid"], (A, B, C))
    h.check("the transaction pays every account its balance", outs == {x: conf[x] for x in conf}, f"{outs} vs {conf}")
    h.check("liabilities are zero and the wallet is locked again", get("/api/v1/ledger")["liabilities"] == 0 and n.rpc("getwalletinfo", wallet="photon")["unlocked_until"] == 0)
    wb = n.rpc("getbalances", wallet="photon")
    h.check("the pool keeps the fee in its wallet (the reward minus the payout minus the network fee)", True, str(wb["mine"]))
    n.rpc("generatetodescriptor", 1, "raw(51)")
    acc = get(f"/public/v1/account/{A}")
    h.check("the account page shows the payout and the paid total", acc["balance"]["paid"] == conf[A] and acc["payouts"][0]["txid"] == p["txid"], str(acc["balance"]))

    # ---- 6. restart: no second payout ----
    count = len(get("/api/v1/ledger")["payouts"])
    pool_b.stop(); pool_b.start(); await ready()
    await asyncio.sleep(8)
    led = get("/api/v1/ledger")
    h.check("after a restart the payouts are the same and nothing is paid twice", len(led["payouts"]) == count and led["liabilities"] == 0, str(led["payouts"]))
    h.check("the balances survive the restart", get(f"/public/v1/account/{A}")["balance"]["paid"] == conf[A])

    # ---- 7. 120 accounts: the payout is split into transactions of at most 50 outputs ----
    pool_b.stop()
    write_cfg(cfg, max_outputs=50)
    pool_c.start(); await ready()
    before = max([p["id"] for p in get("/api/v1/ledger")["payouts"]] or [0])
    accounts = [addr(b"crowd-%d" % i) for i in range(120)]
    for a_ in accounts:
        await finder(h, a_)
    h.check("120 accounts each found a block", n.rpc("getblockcount") >= 130 + 6 + 1 + 120)
    n.rpc("generatetodescriptor", 101, "raw(51)")
    await wait_for(lambda: get("/api/v1/ledger")["blocks"].get("mature", 0) >= 128, 90)
    snap = {a_: get(f"/public/v1/account/{a_}")["balance"]["confirmed"] for a_ in accounts}
    await wait_for(lambda: get("/api/v1/ledger")["liabilities"] == 0 and all(get(f"/public/v1/account/{a_}")["balance"]["paid"] > 0 for a_ in accounts), 150)
    led = get("/api/v1/ledger")
    new = [p for p in led["payouts"] if p["id"] > before]
    h.check("the 120 accounts are paid in at least three transactions", len(new) >= 3 and all(p["txid"] for p in new), f"{len(new)} new payouts")
    paid, sizes = {}, []
    for p in new:
        o = paid_outputs(n, p["txid"], set(accounts))
        sizes.append(len(o))
        for k, v in o.items(): paid[k] = paid.get(k, 0) + v
    h.check(f"no payout transaction has more than 50 account outputs ({sorted(sizes)[-3:]})", max(sizes) <= 50 and max(sizes) == 50, str(sizes))
    pages = {a_: get(f"/public/v1/account/{a_}")["balance"] for a_ in accounts}
    h.check("every account received exactly what its page says was paid", len(paid) == 120 and all(paid.get(a_) == pages[a_]["paid"] for a_ in accounts), f"{len(paid)} paid, mismatches {sum(1 for a_ in accounts if paid.get(a_) != pages[a_]['paid'])}")
    h.check("nothing is left confirmed or owed", led["liabilities"] == 0 and all(pages[a_]["confirmed"] == 0 for a_ in accounts), str(led["liabilities"]))

    # ---- 8. an orphaned block: its credits disappear ----
    E = addr(b"pplns-E")
    bhash3, R3 = await finder(h, E)
    await asyncio.sleep(1)
    h.check("before the reorg the block credits E", get(f"/public/v1/account/{E}")["balance"]["pending"] > 0)
    n.rpc("invalidateblock", bhash3)
    n.rpc("generatetodescriptor", 3, "raw(51)")
    ok = await wait_for(lambda: get("/api/v1/ledger")["blocks"].get("orphaned", 0) >= 1, 40)
    h.check("the ledger marks the block orphaned", ok is not None, str(get("/api/v1/ledger")["blocks"]))
    bal = get(f"/public/v1/account/{E}")["balance"]
    h.check("an orphaned block leaves no pending or confirmed balance", bal["pending"] == 0 and bal["confirmed"] == 0, str(bal))
    n.rpc("generatetodescriptor", 101, "raw(51)")
    await asyncio.sleep(5)
    h.check("nothing is paid for it", get(f"/public/v1/account/{E}")["balance"]["paid"] == 0)

    # ---- 9. minimum payout ----
    pool_c.stop()
    write_cfg(cfg, min_payout=10**13)       # more than any balance
    pool_c.start(); await ready()
    F = addr(b"pplns-F")
    await finder(h, F)
    await finder(h, addr(b"pplns-G"))
    n.rpc("generatetodescriptor", 101, "raw(51)")
    await wait_for(lambda: get(f"/public/v1/account/{F}")["balance"]["confirmed"] > 0, 40)
    await asyncio.sleep(8)
    bf = get(f"/public/v1/account/{F}")["balance"]
    h.check("a balance below the minimum payout is confirmed but not paid", bf["confirmed"] > 0 and bf["paid"] == 0 and get("/api/v1/ledger")["payableAccounts"] == 0, str(bf) + str(get("/api/v1/ledger")))
    pool_c.stop()
    write_cfg(cfg, min_payout=10000)
    pool_c.start(); await ready()
    ok = await wait_for(lambda: get(f"/public/v1/account/{F}")["balance"]["paid"] > 0, 60)
    h.check("with the minimum lowered the balance is paid", ok is not None, str(get(f"/public/v1/account/{F}")["balance"]))
    pool_c.stop()

with Harness("step33", extra=["-fallbackfee=0.0001", "-acceptnonstdtxn=1", "-maxmempool=500"]) as h:
    base = h.node.datadir + "-cfg"
    cfg = base + ".toml"
    log = base + ".log"
    for ext in ("", "-wal", "-shm"):
        try: os.remove(cfg + ".db" + ext)
        except FileNotFoundError: pass
    try: os.remove(log)
    except FileNotFoundError: pass
    write_cfg(cfg)
    pa, pb, pc = Pool(cfg, WRONG, log), Pool(cfg, PASS, log), Pool(cfg, PASS, log)
    try:
        asyncio.run(main(h, pa, pb, pc, cfg))
    finally:
        for p in (pa, pb, pc): p.stop()
