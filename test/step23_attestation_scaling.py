"""Does the attestation cost depend on the size of the UTXO set, or only on what a block creates and spends?

The claim (protocol guideline 7.2): the node keeps the attestation as an incremental accumulator, so a block costs time in proportion to the outputs
it creates and spends, not to the size of the set. Measured on regtest with the node that has the Photon patch:
  1. the set grows from about 2,000 to about 72,000 outputs; a block of the same shape is built and submitted at each size
  2. at the largest size, blocks of 50, 200 and 800 transactions are submitted
  3. for contrast, `gettxoutsetinfo muhash` (a full pass over the set) is timed at each size
Everything is cache resident (regtest sets are small); disk effects of a real chainstate are not part of this test."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import statistics, struct, time
from decimal import Decimal
from photon_testlib import *
from photon_testlib import block_from_batch, solve_parts

REPS = 5

def fanout(prev, value, outputs=100, fee=5000):
    each = (value - fee) // outputs
    raw = struct.pack("<i", 2) + varint(1) + prev + struct.pack("<I", 0) + b"\x00" + struct.pack("<I", 0xfffffffd) + varint(outputs) + b"".join(txout(each, b"\x51") for _ in range(outputs)) + struct.pack("<I", 0)
    return raw, each

with Harness("step23", blocks=860, extra=["-acceptnonstdtxn=1", "-maxmempool=500"]) as h:
    n = h.node
    coinbases = []
    for height in range(1, 760):
        cb = n.rpc("getblock", n.rpc("getblockhash", height), 2)["tx"][0]
        coinbases.append((bytes.fromhex(cb["txid"])[::-1], int(Decimal(str(cb["vout"][0]["value"])) * LEPTON)))
    pos = 0

    def grow(count):
        """count fan-outs of 100 outputs each; returns the new outputs"""
        global pos
        new = []
        for _ in range(count):
            prev, value = coinbases[pos]; pos += 1
            raw, each = fanout(prev, value)
            n.rpc("sendrawtransaction", raw.hex())
            new += [(sha256d(raw), i, each) for i in range(100)]
        while n.rpc("getmempoolinfo")["size"] > 0:
            n.rpc("generatetodescriptor", 1, "raw(51)")
        return new

    def work_block(outputs, ntx):
        """ntx transactions that each spend one output and create two; returns (ms of getattestationbatch, ms of submitblock)"""
        for txid, i, v in outputs[:ntx]:
            raw, _ = make_spend(txid, i, v, outputs=2)
            n.rpc("sendrawtransaction", raw.hex())
        t0 = time.perf_counter()
        bt = n.rpc("getattestationbatch", {"candidates": [{"outputs": [{"scriptPubKey": p2wpkh_script(1).hex()}]}]})
        t_batch = (time.perf_counter() - t0) * 1000
        datas = [bytes.fromhex(x["data"]) for x in bt["transactions"]]
        assert len(datas) == ntx, (len(datas), ntx)
        hexblock = solve_parts(bt, block_from_batch(bt, bt["candidates"][0], datas))
        t0 = time.perf_counter()
        r = n.rpc("submitblock", hexblock)
        t_submit = (time.perf_counter() - t0) * 1000
        assert r is None, r
        return t_batch, t_submit

    def txouts(): return n.rpc("gettxoutsetinfo", "muhash")["txouts"]
    def setinfo_ms():
        t0 = time.perf_counter(); n.rpc("gettxoutsetinfo", "muhash"); return (time.perf_counter() - t0) * 1000

    rows = []
    for name, fanouts in (("A", 15), ("B", 200), ("C", 500)):
        pool = grow(fanouts)
        size = txouts()
        batch, submit = [], []
        used = 0
        for _ in range(REPS):
            tb, ts = work_block(pool[used:], 200); used += 200
            batch.append(tb); submit.append(ts)
        info = statistics.median([setinfo_ms() for _ in range(3)])
        rows.append((name, size, statistics.median(batch), statistics.median(submit), info))
        print(f"  info stage {name}: {size:>6} outputs in the set | getattestationbatch {rows[-1][2]:6.1f} ms | submitblock {rows[-1][3]:6.1f} ms (200 txs) | gettxoutsetinfo {info:7.1f} ms")
        last_pool = pool[used:]

    a, c = rows[0], rows[-1]
    h.check(f"the set grew from {a[1]} to {c[1]} outputs (a factor of {c[1] / a[1]:.0f})", c[1] / a[1] > 20)
    h.check(f"submitting a block of the same shape costs the same at both sizes ({a[3]:.1f} ms against {c[3]:.1f} ms)", c[3] <= a[3] * 1.5 + 5)
    h.check(f"so does the attestation of the template ({a[2]:.1f} ms against {c[2]:.1f} ms)", c[2] <= a[2] * 1.5 + 5)
    h.check(f"a full pass over the set does grow with the size (gettxoutsetinfo {a[4]:.1f} ms -> {c[4]:.1f} ms)", c[4] >= a[4] * 3)

    # shape at the largest size
    pool = grow(30)
    shape = {}
    for ntx in (50, 200, 800):
        times = []
        for _ in range(3):
            if len(pool) < ntx: pool += grow(10)
            times.append(work_block(pool, ntx)[1]); pool = pool[ntx:]
        shape[ntx] = statistics.median(times)
        print(f"  info {ntx:>3} transactions (each spends 1, creates 2 outputs): submitblock {shape[ntx]:6.1f} ms = {shape[ntx] / ntx:.3f} ms per transaction")
    per = {k: v / k for k, v in shape.items()}
    h.check("the cost grows with what the block creates and spends (800 transactions cost more than 3 times as much as 50)", shape[800] > 3 * shape[50])
    h.check("and about in proportion (per-transaction cost within a factor of 3 between 200 and 800 transactions)", per[800] < 3 * per[200] and per[200] < 3 * per[800])
