import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from photon_testlib import *

with Harness("step01") as h:
    n = h.node
    o = Oracle(n, [b"\x51"])
    info = n.rpc("gettxoutsetinfo", "muhash")
    mine = digest_of(o.set_hash_num(o.coins))[::-1].hex()
    h.check("oracle MuHash of scanned UTXO set equals gettxoutsetinfo muhash", mine == info["muhash"], mine[:16])
    h.check("scanned coin count equals node coin count", len(o.coins) == info["txouts"], f"{len(o.coins)}")

    t = n.gbt()
    cb = build_coinbase(t, b"\x51")
    base = o.base_after_txs(t["height"], [])
    dg = o.candidate_digest(base, t["height"], cb["stripped_txid"], cb["value"], b"\x51")
    expected = attestation_script(t["height"], dg)
    h.check("oracle attestation equals node attestation (empty block, OP_TRUE payout)", expected == cb["node_attestation"],
            f"oracle {expected.hex()[-16:]} node {cb['node_attestation'].hex()[-16:]}")
