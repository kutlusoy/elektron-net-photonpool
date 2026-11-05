import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import struct
from photon_testlib import *

# The core promise of the batch patch: a node WITHOUT the patch accepts every block assembled from the batch response, and rejects a block whose attestation is wrong.
# Blocks come from a patched node (ELEKTROND, the harness node) and are submitted to a second, unpatched node (UNPATCHED) that has the same chain.

UNPATCHED = os.environ.get("UNPATCHED", "elektrond")

def tamper_attestation(cand):
    """Same coinbase with the last byte of the attestation digest flipped."""
    c = dict(cand); att = bytes.fromhex(cand["attestation_script"]); bad = att[:-1] + bytes([att[-1] ^ 1])
    c["coinbase_hex"] = cand["coinbase_hex"].replace(att.hex(), bad.hex(), 1)
    assert c["coinbase_hex"] != cand["coinbase_hex"]
    return c

with Harness("step29", extra=["-acceptnonstdtxn=1"]) as h:
    a = h.node
    b = Node(UNPATCHED, os.environ.get("PHOTON_DATADIR", "/tmp/photon-test") + "-step29-b", ["-acceptnonstdtxn=1"], rpcport=18995, port=18996)
    b.start()
    try:
        def sync():
            for height in range(b.rpc("getblockcount") + 1, a.rpc("getblockcount") + 1):
                b.rpc("submitblock", a.rpc("getblock", a.rpc("getblockhash", height), 0))
            return b.rpc("getbestblockhash") == a.rpc("getbestblockhash")
        h.check("the unpatched node has the same chain", sync())
        h.check("the unpatched node has no getattestationbatch", "getattestationbatch" not in b.rpc("help"))
        # a spend in the mempool, so that the template is not empty
        cb = a.rpc("getblock", a.rpc("getblockhash", 1), 2)["tx"][0]
        from decimal import Decimal
        tx, _ = make_spend(bytes.fromhex(cb["txid"])[::-1], 0, int(Decimal(str(cb["vout"][0]["value"])) * LEPTON), outputs=2)
        a.rpc("sendrawtransaction", tx.hex())
        s = [p2wpkh_script(i) for i in range(1, 6)]
        reward = a.rpc("getattestationbatch", {"candidates": [{"outputs": [{"scriptPubKey": s[0].hex()}]}]})["coinbasevalue"]
        variants = [
            {"outputs": [{"scriptPubKey": s[0].hex()}]},
            {"outputs": [{"scriptPubKey": s[1].hex(), "value": reward // 3}, {"scriptPubKey": s[2].hex()}]},
            {"outputs": [{"scriptPubKey": s[3].hex()}], "tail_outputs": [{"scriptPubKey": s[4].hex(), "value": 0}]},
            {"outputs": [{"scriptPubKey": s[i % 5].hex(), "value": 1000 + i} for i in range(40)] + [{"scriptPubKey": s[0].hex()}], "script_sig_suffix": "ab" * 8},
        ]
        accepted = 0
        for round_ in range(3):
            bt = a.rpc("getattestationbatch", {"candidates": variants})
            datas = [bytes.fromhex(t["data"]) for t in bt["transactions"]]
            for i, c in enumerate(bt["candidates"]):
                if i != round_ % len(variants): continue
                blk = solve_parts(bt, block_from_batch(bt, c, datas))
                r = b.rpc("submitblock", blk)
                if r is None: accepted += 1
                a.rpc("submitblock", blk)
                h.check(f"round {round_}: the unpatched node accepts the block of variant {i}", r is None and b.rpc("getbestblockhash") == a.rpc("getbestblockhash"), str(r))
        # submitblock returning null is not the proof by itself: the block must be part of the ACTIVE chain of the unpatched node, which must hold exactly the same
        # UTXO set (MuHash over all unspent outputs) as the node that built it
        tips = b.rpc("getchaintips")
        h.check("the tip of the unpatched node is the active chain tip with the same hash and height", any(t["status"] == "active" and t["hash"] == a.rpc("getbestblockhash") and t["height"] == a.rpc("getblockcount") for t in tips), str(tips[:2]))
        h.check("both nodes hold the same UTXO set (MuHash)", b.rpc("gettxoutsetinfo", "muhash")["muhash"] == a.rpc("gettxoutsetinfo", "muhash")["muhash"])
        # wrong attestation must be rejected
        bt = a.rpc("getattestationbatch", {"candidates": variants[:1]})
        datas = [bytes.fromhex(t["data"]) for t in bt["transactions"]]
        bad = solve_parts(bt, block_from_batch(bt, tamper_attestation(bt["candidates"][0]), datas))
        before = b.rpc("getbestblockhash")
        r = b.rpc("submitblock", bad)
        h.check("the unpatched node rejects a block with a wrong attestation", r is not None and b.rpc("getbestblockhash") == before, str(r))
        r2 = a.rpc("submitblock", bad)
        h.check("so does the patched node", r2 is not None, str(r2))
    finally:
        b.stop()
