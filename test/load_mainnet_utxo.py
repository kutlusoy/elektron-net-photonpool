import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, re, struct, threading, time
from decimal import Decimal
from photon_testlib import *

# Load test of getattestationbatch against a UTXO set of mainnet size (needs a node with the RPC: ELEKTROND of the branch photonpool of elektron-net).
# A regtest chain is filled with SIZES UTXOs (outputs of the size of a P2WPKH output) by fanning out mature coinbases, then, at every size:
#   - latency of getattestationbatch for CANDS candidates (p50, p95, max over REPS calls) and the time cs_main was held (from debug.log, -debug=rpc),
#   - how long a block takes to create and connect (generatetodescriptor 1) with and without two heavy batch calls running in parallel,
#   - the latency of a light RPC (getblockchaininfo) while heavy batch calls run,
#   - resident memory of the node and the cost of one full pass over the set (gettxoutsetinfo muhash, once).
# The node runs with the default database cache so that a large set does not fit in memory. Settings by environment variables:
#   SIZES=300000,2000000,5000000   UTXO counts to measure (ascending)       CANDS=1,64,512,4096     candidate counts     REPS=8     calls per measurement
#   TXS=2000   transactions in the mempool while measuring (they are in the block template the RPC builds, so its attestation base state has to apply them)
#   OUT=/path/result.json          write the numbers                       PER_TX=2500   outputs per fan-out transaction (the mempool cluster limit is about 100 kvB)
SIZES = [int(x) for x in os.environ.get("SIZES", "300000,1000000").split(",")]
CANDS = [int(x) for x in os.environ.get("CANDS", "1,64,512,4096").split(",")]
REPS = int(os.environ.get("REPS", "8"))
TXS = int(os.environ.get("TXS", "2000"))   # transactions waiting in the mempool during the measurements (a full block of a busy chain)
PER_TX = int(os.environ.get("PER_TX", "2500"))
OUT = os.environ.get("OUT", "")
EACH = 100    # leptons per fan-out output (there is no dust limit in consensus)
FEE_RATE = 2  # leptons per byte of the fan-out transaction

def p2wpkh(i): return b"\x00\x14" + hashlib.sha256(b"load-%d" % i).digest()[:20]
def pct(v, q):
    s = sorted(v); return s[min(len(s) - 1, int(len(s) * q))]
def rss_mb(pid):
    for l in open(f"/proc/{pid}/status"):
        if l.startswith("VmRSS"): return int(l.split()[1]) // 1024
    return 0

spendables = []   # (txid in internal byte order, output index, value) of confirmed outputs that anybody can spend (OP_TRUE)

def fanout_tx(prev_txid_le, prev_idx, value):
    size = 4 + 1 + 41 + 3 + PER_TX * (8 + 1 + 22) + 2 * (8 + 1 + 1) + 4
    rest = value - size * FEE_RATE - PER_TX * EACH
    assert rest > 1000, f"not enough value for a fan-out ({value} leptons)"
    outs = b"".join(txout(EACH, p2wpkh(i)) for i in range(PER_TX)) + txout(rest // 2, b"\x51") + txout(rest - rest // 2, b"\x51")
    raw = struct.pack("<i", 2) + varint(1) + prev_txid_le + struct.pack("<I", prev_idx) + b"\x00" + struct.pack("<I", 0xfffffffd) + varint(PER_TX + 2) + outs + struct.pack("<I", 0)
    txid = sha256d(raw)
    return raw, [(txid, PER_TX, rest // 2), (txid, PER_TX + 1, rest - rest // 2)]

def fill_mempool(n_node, count):
    sent = 0
    while sent < count and spendables:
        txid, idx, value = spendables.pop(0)
        half = (value - 300) // 2
        raw = struct.pack("<i", 2) + varint(1) + txid + struct.pack("<I", idx) + b"\x00" + struct.pack("<I", 0xfffffffd) + varint(2) + txout(half, b"\x51") + txout(half, b"\x51") + struct.pack("<I", 0)
        n_node.rpc("sendrawtransaction", raw.hex())
        spendables.append((sha256d(raw), 0, half)); spendables.append((sha256d(raw), 1, half))
        sent += 1
    return sent

def batch_req(n):
    return {"candidates": [{"outputs": [{"scriptPubKey": p2wpkh(i).hex()}]} for i in range(n)]}

with Harness("loadutxo", blocks=0, extra=["-acceptnonstdtxn=1", "-debug=rpc", "-persistmempool=0", "-maxmempool=2000"]) as h:
    n = h.node
    log_path = os.path.join(n.datadir, "regtest", "debug.log")
    utxos = 0              # our count of unspent outputs
    results = []

    def mine(count):
        for _ in range(0, count, 100): n.rpc("generatetodescriptor", min(100, count - _), "raw(51)")

    pool = spendables     # anyone-can-spend outputs to fan out from and, in the measurements, to spend in the mempool
    def fill_to(target):
        global utxos
        t0 = time.time()
        if n.rpc("getblockcount") < 141:
            mine(141)
            utxos = 141
            for hgt in range(1, 41):                       # the first coinbases carry the full subsidy
                cb = n.rpc("getblock", n.rpc("getblockhash", hgt), 2)["tx"][0]
                pool.append((bytes.fromhex(cb["txid"])[::-1], 0, int(Decimal(str(cb["vout"][0]["value"])) * LEPTON)))
        while n.rpc("getmempoolinfo")["size"] > 0:
            n.rpc("generatetodescriptor", 1, "raw(51)"); utxos += 1
        while utxos < target:
            todo = min((target - utxos) // (PER_TX + 1) + 1, 250, len(pool))
            new = []
            for _ in range(todo):
                txid, idx, value = pool.pop(0)
                raw, outs = fanout_tx(txid, idx, value)
                n.rpc("sendrawtransaction", raw.hex()); new += outs
            for _ in range(2000):
                if n.rpc("getmempoolinfo")["size"] == 0: break
                n.rpc("generatetodescriptor", 1, "raw(51)"); utxos += 1
            pool.extend(new); utxos += todo * (PER_TX + 1)
            print(f"  info filled {utxos} of {target} outputs, height {n.rpc('getblockcount')}, {len(pool)} spendable, {time.time() - t0:.0f} s", flush=True)

    def read_hold_ms(since):
        out = []
        try:
            with open(log_path, errors="replace") as f:
                f.seek(since)
                for l in f:
                    m = re.search(r"cs_main held for (\d+) ms \((\d+) candidates\)", l)
                    if m: out.append((int(m.group(1)), int(m.group(2))))
        except FileNotFoundError: pass
        return out

    for size in SIZES:
        fill_to(size)
        r = {"utxos_target": size, "height": n.rpc("getblockcount"), "rss_mb": rss_mb(n.proc.pid)}
        t0 = time.time(); info = n.rpc("gettxoutsetinfo", "muhash"); r["txoutset_pass_s"] = round(time.time() - t0, 2); r["utxos_actual"] = info["txouts"]
        print(f"[size {r['utxos_actual']}] height {r['height']}, rss {r['rss_mb']} MB, one pass over the set (gettxoutsetinfo) {r['txoutset_pass_s']} s")
        r["batch"] = {}
        r["mempool_txs"] = fill_mempool(n, TXS)
        for c in CANDS:
            lat, size_before = [], os.path.getsize(log_path) if os.path.exists(log_path) else 0
            req = batch_req(c)
            for _ in range(REPS):
                t0 = time.time(); n.rpc("getattestationbatch", req); lat.append((time.time() - t0) * 1000)
            hold = [x[0] for x in read_hold_ms(size_before) if x[1] == c]
            r["batch"][c] = {"p50_ms": round(pct(lat, 0.5), 1), "p95_ms": round(pct(lat, 0.95), 1), "max_ms": round(max(lat), 1), "cs_main_max_ms": max(hold) if hold else None}
            print(f"   {c:5d} candidates: p50 {r['batch'][c]['p50_ms']:8.1f} ms  p95 {r['batch'][c]['p95_ms']:8.1f} ms  max {r['batch'][c]['max_ms']:8.1f} ms  cs_main held up to {r['batch'][c]['cs_main_max_ms']} ms", flush=True)
        # block creation and connection, idle and under heavy batch load
        def block_ms():
            fill_mempool(n, min(TXS, 500))
            t0 = time.time(); n.rpc("generatetodescriptor", 1, "raw(51)"); return (time.time() - t0) * 1000
        idle = [block_ms() for _ in range(5)]
        stop = threading.Event(); heavy = batch_req(max(CANDS))
        def load():
            while not stop.is_set():
                try: n.rpc("getattestationbatch", heavy)
                except RuntimeError: time.sleep(0.05)
        ths = [threading.Thread(target=load) for _ in range(3)]; [t.start() for t in ths]
        time.sleep(0.5)
        busy, light = [], []
        for _ in range(5):
            busy.append(block_ms())
            t0 = time.time(); n.rpc("getblockchaininfo"); light.append((time.time() - t0) * 1000)
        stop.set(); [t.join() for t in ths]
        r["block_ms_idle_p50"], r["block_ms_under_load_p50"], r["block_ms_under_load_max"] = round(pct(idle, 0.5), 1), round(pct(busy, 0.5), 1), round(max(busy), 1)
        r["getblockchaininfo_ms_under_load_max"] = round(max(light), 1)
        print(f"   block create+connect: idle p50 {r['block_ms_idle_p50']} ms, with heavy batch calls running p50 {r['block_ms_under_load_p50']} ms (max {r['block_ms_under_load_max']} ms); getblockchaininfo max {r['getblockchaininfo_ms_under_load_max']} ms")
        results.append(r)
        h.check(f"size {r['utxos_actual']}: the node keeps connecting blocks and answering under heavy batch load", r["block_ms_under_load_max"] < 30000 and r["getblockchaininfo_ms_under_load_max"] < 5000)
    if OUT: json.dump(results, open(OUT, "w"), indent=1)
