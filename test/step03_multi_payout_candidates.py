import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from photon_testlib import *

with Harness("step03") as h:
    n = h.node
    payouts = [b"\x51"] + [p2wpkh_script(i) for i in range(1, 6)]
    o = Oracle(n, payouts)
    t = n.gbt()
    height = t["height"]
    base = o.base_after_txs(height, [])
    atts, cbs = [], []
    for p in payouts:
        probe = build_coinbase(t, p)
        dg = o.candidate_digest(base, height, probe["stripped_txid"], probe["value"], p)
        atts.append(dg)
        cbs.append(build_coinbase(t, p, attestation=attestation_script(height, dg)))
    h.check("six payout scripts give six different attestations", len(set(atts)) == 6)
    h.check("OP_TRUE candidate equals node attestation", cbs[0]["nowit"] == build_coinbase(t, b"\x51")["nowit"])
    for i, cb in enumerate(cbs):
        r = proposal(n, block_hex_unsolved(*assemble_block(t, cb)))
        h.check(f"proposal accepts candidate {i} (payout {payouts[i].hex()[:12]}..)", r is None, str(r))
    wrong = build_coinbase(t, payouts[3], attestation=attestation_script(height, atts[2]))
    h.check("attestation of another payout is rejected", proposal(n, block_hex_unsolved(*assemble_block(t, wrong))) == "bad-utxo-attestation")
    h.check("submitblock accepts a P2WPKH payout candidate", submit(n, t, cbs[4]) is None)
    blk = n.rpc("getblock", n.rpc("getbestblockhash"), 2)
    h.check("accepted block pays the P2WPKH script", blk["tx"][0]["vout"][0]["scriptPubKey"]["hex"] == payouts[4].hex())

    o2 = Oracle(n, payouts)
    t2 = n.gbt()
    base2 = o2.base_after_txs(t2["height"], [])
    p = payouts[2]
    probe = build_coinbase(t2, p)
    dg = o2.candidate_digest(base2, t2["height"], probe["stripped_txid"], probe["value"], p)
    cb = build_coinbase(t2, p, attestation=attestation_script(t2["height"], dg))
    h.check("next height: oracle (now including a P2WPKH coinbase coin in the set) is accepted", submit(n, t2, cb) is None)
