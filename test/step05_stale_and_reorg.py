import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from photon_testlib import *

with Harness("step05") as h:
    n = h.node
    payouts = [b"\x51", p2wpkh_script(1), p2wpkh_script(2)]
    o = Oracle(n, payouts)
    t_old = n.gbt()
    height = t_old["height"]
    base = o.base_after_txs(height, [])
    def cand(t, p):
        probe = build_coinbase(t, p)
        return build_coinbase(t, p, attestation=attestation_script(t["height"], o.candidate_digest(base, t["height"], probe["stripped_txid"], probe["value"], p)))
    cb_a, cb_b = cand(t_old, payouts[0]), cand(t_old, payouts[1])
    old_tip = n.rpc("getbestblockhash")
    h.check("block A (payout 0) on the old tip is accepted", submit(n, t_old, cb_a) is None)
    tip_a = n.rpc("getbestblockhash")
    r = proposal(n, block_hex_unsolved(*assemble_block(t_old, cb_b)))
    print(f"  info proposal of the stale candidate B -> {r!r}")
    h.check("proposal does not claim the stale candidate is a clean success", r is not None, str(r))
    r = submit(n, t_old, cb_b)
    print(f"  info submitblock of stale candidate B -> {r!r}")
    h.check("stale candidate B is not made the best block", n.rpc("getbestblockhash") == tip_a)
    n.rpc("invalidateblock", tip_a)
    tip_b = n.rpc("getbestblockhash")
    h.check("after invalidating A the node reorgs onto B (attested against the old parent)", tip_b != tip_a and tip_b != old_tip)
    blk = n.rpc("getblock", tip_b, 2)
    h.check("best block pays payout 1", blk["tx"][0]["vout"][0]["scriptPubKey"]["hex"] == payouts[1].hex())
    n.rpc("reconsiderblock", tip_a)
    h.check("chain stays consistent after reconsider", n.rpc("getblockcount") == height)
    t_new = n.gbt()
    o2 = Oracle(n, payouts)
    base2 = o2.base_after_txs(t_new["height"], [])
    probe = build_coinbase(t_new, b"\x51")
    cb = build_coinbase(t_new, b"\x51", attestation=attestation_script(t_new["height"], o2.candidate_digest(base2, t_new["height"], probe["stripped_txid"], probe["value"], b"\x51")))
    h.check("fresh candidate on the new tip is accepted", submit(n, t_new, cb) is None)
