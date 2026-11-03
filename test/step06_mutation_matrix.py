import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from photon_testlib import *

with Harness("step06") as h:
    n = h.node
    o = Oracle(n, [b"\x51"])
    t = n.gbt()
    height = t["height"]
    base = o.base_after_txs(height, [])

    def attest(p=b"\x51", extra=(), value=None, spendable=(), h_off=0):
        probe = build_coinbase(t, p, extra, payout_value=value)
        dg = o.candidate_digest(base, height, probe["stripped_txid"], probe["value"], p, spendable)
        return attestation_script(height + h_off, dg)

    def prop(cb): return proposal(n, block_hex_unsolved(*assemble_block(t, cb)))

    h.check("baseline accepted", prop(build_coinbase(t, b"\x51", attestation=attest())) is None)
    cases = [
        ("payout value minus 1 with the original attestation", build_coinbase(t, b"\x51", payout_value=t["coinbasevalue"] - 1, attestation=attest()), "bad-utxo-attestation"),
        ("payout value minus 1 with recomputed attestation (valid only if cb-amount rule allows)", build_coinbase(t, b"\x51", payout_value=t["coinbasevalue"] - 1, attestation=attest(value=t["coinbasevalue"] - 1)), None),
        ("scriptSig suffix with original attestation", build_coinbase(t, b"\x51", script_sig_suffix=b"\x01", attestation=attest()), "bad-utxo-attestation"),
        ("attestation computed for height-1", build_coinbase(t, b"\x51", attestation=attest(h_off=-1)), "missing-utxo-attestation"),
        ("wrong digest of the right height", build_coinbase(t, b"\x51", attestation=attestation_script(height, b"\x11" * 32)), "bad-utxo-attestation"),
        ("zero-value spendable extra output, attestation without it", build_coinbase(t, b"\x51", [b"\x51"]), "bad-utxo-attestation"),
        ("zero-value spendable extra output, attestation with its coin", None, None),
    ]
    probe = build_coinbase(t, b"\x51", [b"\x51"])
    cases[-1] = (cases[-1][0], build_coinbase(t, b"\x51", [b"\x51"], attestation=attest(extra=[b"\x51"], spendable=[(2, (0, b"\x51"))])), None)
    for name, cb, expected in cases:
        r = prop(cb)
        h.check(f"{name} -> {expected!r}", r == expected, repr(r))
    no_att = build_coinbase(t, b"\x51")
    no_att["nowit"] = no_att["nowit"]
