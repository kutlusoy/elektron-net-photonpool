import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import random, time
from photon_testlib import *

COUNT = 500
SAMPLE = 25

with Harness("step07") as h:
    n = h.node
    payouts = [b"\x51"] + [p2wpkh_script(i) for i in range(1, COUNT)]
    o = Oracle(n, [b"\x51"])
    t = n.gbt()
    height = t["height"]
    t0 = time.perf_counter()
    base = o.base_after_txs(height, [])
    t_base = time.perf_counter() - t0
    t0 = time.perf_counter()
    cbs, digests = [], []
    for p in payouts:
        probe = build_coinbase(t, p)
        dg = o.candidate_digest(base, height, probe["stripped_txid"], probe["value"], p)
        digests.append(dg)
        cbs.append(build_coinbase(t, p, attestation=attestation_script(height, dg)))
    t_cands = time.perf_counter() - t0
    print(f"  info python oracle: base {t_base*1000:.1f} ms for {len(o.coins)} coins, {COUNT} candidates in {t_cands*1000:.0f} ms ({t_cands/COUNT*1000:.2f} ms each)")
    h.check(f"{COUNT} distinct attestations", len(set(digests)) == COUNT)
    random.seed(1)
    t0 = time.perf_counter()
    bad = [i for i in random.sample(range(COUNT), SAMPLE) if proposal(n, block_hex_unsolved(*assemble_block(t, cbs[i]))) is not None]
    t_prop = time.perf_counter() - t0
    print(f"  info {SAMPLE} proposals in {t_prop*1000:.0f} ms ({t_prop/SAMPLE*1000:.1f} ms each)")
    h.check(f"{SAMPLE} sampled candidates accepted by proposal", not bad, str(bad))
    t0 = time.perf_counter()
    r = [submit(n, t, cbs[i]) for i in [COUNT - 1]]
    h.check("last candidate mined and accepted", r == [None])
