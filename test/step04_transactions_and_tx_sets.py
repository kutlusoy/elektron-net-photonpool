import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from photon_testlib import *

def coinbase_of(n, height):
    blk = n.rpc("getblock", n.rpc("getblockhash", height), 2)
    cb = blk["tx"][0]
    return bytes.fromhex(cb["txid"])[::-1], int(Decimal(cb["vout"][0]["value"]) * LEPTON)

with Harness("step04", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    txid_a, val_a = coinbase_of(n, 1)
    txid_b, val_b = coinbase_of(n, 2)
    tx1, outs1 = make_spend(txid_a, 0, val_a, outputs=2)
    tx2, outs2 = make_spend(sha256d(tx1), 0, outs1[0][0], outputs=2)
    tx3, outs3 = make_spend(txid_b, 0, val_b, outputs=2)
    for name, raw in (("parent", tx1), ("child of parent", tx2), ("independent two-output tx", tx3)):
        try:
            n.rpc("sendrawtransaction", raw.hex()); h.check(f"mempool accepts {name}", True)
        except Exception as e:
            h.check(f"mempool accepts {name}", False, str(e))
    payouts = [b"\x51", p2wpkh_script(1), p2wpkh_script(2)]
    o = Oracle(n, payouts)
    t = n.gbt()
    height = t["height"]
    datas = [bytes.fromhex(x["data"]) for x in t["transactions"]]
    h.check("template contains the three transactions", len(datas) == 3, str(len(datas)))

    def value_for(txs): return t["coinbasevalue"] - 1000 * (3 - len(txs))

    def cand(p, txs, extra=()):
        base = o.base_after_txs(height, txs)
        probe = build_coinbase(t, p, extra, tx_datas=txs, payout_value=value_for(txs))
        return build_coinbase(t, p, extra, tx_datas=txs, payout_value=value_for(txs), attestation=attestation_script(height, o.candidate_digest(base, height, probe["stripped_txid"], probe["value"], p)))

    def prop(cb, txs): return proposal(n, block_hex_unsolved(*assemble_block(t, cb, txs)))

    cb_node = build_coinbase(t, b"\x51")
    h.check("witness commitment recomputed locally equals the node's", witness_commitment_script(datas) == cb_node["wc"])
    h.check("oracle attestation equals node attestation with an in-block dependent chain",
            cand(b"\x51", datas)["nowit"] == cb_node["nowit"])
    for i, p in enumerate(payouts):
        r = prop(cand(p, datas), datas)
        h.check(f"proposal accepts payout candidate {i} with all three transactions", r is None, str(r))
    by_id = {sha256d(d): d for d in datas}
    parent, child, indep = by_id[sha256d(tx1)], by_id[sha256d(tx2)], by_id[sha256d(tx3)]
    subsets = {"independent tx dropped": [parent, child], "child dropped (parent only)": [parent],
               "valid reordering of the same set": next(o for o in ([indep, parent, child], [parent, indep, child], [parent, child, indep]) if o != datas)}
    for name, txs in subsets.items():
        stale = build_coinbase(t, b"\x51", tx_datas=txs, payout_value=value_for(txs), attestation=cb_node["node_attestation"])
        r = prop(stale, txs)
        h.check(f"{name}: node attestation reused with consistent witness commitment is rejected", r == "bad-utxo-attestation", str(r))
        r = prop(cand(b"\x51", txs), txs)
        h.check(f"{name}: recomputed witness commitment and attestation is accepted", r is None, str(r))
    stale_wc = prop(build_coinbase(t, b"\x51", payout_value=value_for([parent, child]), attestation=cb_node["node_attestation"]), [parent, child])
    h.check("tx set changed but template witness commitment kept is rejected", stale_wc == "bad-witness-merkle-match", str(stale_wc))
    h.check("submitblock accepts a P2WPKH candidate with the full transaction set", submit(n, t, cand(payouts[1], datas), datas) is None)
    blk = n.rpc("getblock", n.rpc("getbestblockhash"), 1)
    h.check("accepted block has 4 transactions", len(blk["tx"]) == 4, str(len(blk["tx"])))
    h.check("mempool is empty afterwards", n.rpc("getmempoolinfo")["size"] == 0)
