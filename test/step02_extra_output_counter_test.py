import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from photon_testlib import *

with Harness("step02") as h:
    n = h.node
    o = Oracle(n, [b"\x51"])
    t = n.gbt()
    height = t["height"]
    base = o.base_after_txs(height, [])
    tag = op_return(b"pool-tag-test")

    def attested(extra):
        probe = build_coinbase(t, b"\x51", extra)
        dg = o.candidate_digest(base, height, probe["stripped_txid"], probe["value"], b"\x51")
        return build_coinbase(t, b"\x51", extra, attestation=attestation_script(height, dg))

    def prop(cb): return proposal(n, block_hex_unsolved(*assemble_block(t, cb)))

    cb0 = build_coinbase(t, b"\x51")
    h.check("proposal accepts the unmodified node template", prop(cb0) is None)
    bad = bytearray(cb0["node_attestation"]); bad[-1] ^= 1
    h.check("proposal rejects a flipped attestation bit (proposal validates attestation)",
            prop(build_coinbase(t, b"\x51", attestation=bytes(bad))) == "bad-utxo-attestation")
    h.check("extra OP_RETURN with the node attestation is rejected", prop(build_coinbase(t, b"\x51", [tag])) == "bad-utxo-attestation")

    cb1 = attested([tag])
    h.check("extra OP_RETURN with oracle attestation: proposal accepts", prop(cb1) is None)
    h.check("extra OP_RETURN with oracle attestation: submitblock accepts", submit(n, t, cb1) is None)
    h.check("tip advanced", n.rpc("getblockcount") == height)
    blk = n.rpc("getblock", n.rpc("getbestblockhash"), 2)
    outs = blk["tx"][0]["vout"]
    h.check("accepted coinbase carries 4 outputs (payout, attestation, witness commitment, tag)", len(outs) == 4, str(len(outs)))
    h.check("tag output is last", outs[-1]["scriptPubKey"]["hex"] == tag.hex())
