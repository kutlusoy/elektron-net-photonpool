import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, struct, time
from decimal import Decimal
from photon_testlib import *

def coinbase_of(n, height):
    blk = n.rpc("getblock", n.rpc("getblockhash", height), 2)
    cb = blk["tx"][0]
    return bytes.fromhex(cb["txid"])[::-1], int(Decimal(cb["vout"][0]["value"]) * LEPTON)

def p2wpkh(i): return p2wpkh_script(i)
def opret(payload): return op_return(payload)

def build_block(bt, cand, tx_datas):
    """Assemble a block from the batch response (coinbase from the node plus the template fields)."""
    cb_nowit = bytes.fromhex(cand["coinbase_hex"])
    # witness serialization: marker/flag after the version, witness stack before locktime
    body = cb_nowit[4:-4]
    reserved = bytes.fromhex(bt.get("coinbase_witness", "00" * 32))[::-1]
    wit = cb_nowit[:4] + b"\x00\x01" + body + varint(1) + varint(32) + reserved + cb_nowit[-4:]
    leaves = [sha256d(cb_nowit)] + [sha256d(d) for d in tx_datas]
    while len(leaves) > 1:
        if len(leaves) % 2: leaves.append(leaves[-1])
        leaves = [sha256d(leaves[i] + leaves[i + 1]) for i in range(0, len(leaves), 2)]
    head = struct.pack("<i", bt["version"]) + bytes.fromhex(bt["tip"])[::-1] + leaves[0] + struct.pack("<I", bt["curtime"]) + bytes.fromhex(bt["bits"])[::-1]
    return head, wit, b"".join(tx_datas), 1 + len(tx_datas)

with Harness("step14", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    txid_a, val_a = coinbase_of(n, 1)
    txid_b, val_b = coinbase_of(n, 2)
    tx1, outs1 = make_spend(txid_a, 0, val_a, outputs=2)
    tx2, _ = make_spend(sha256d(tx1), 0, outs1[0][0], outputs=2)
    tx3, _ = make_spend(txid_b, 0, val_b, outputs=2)
    for raw in (tx1, tx2, tx3): n.rpc("sendrawtransaction", raw.hex())

    scripts = [b"\x51"] + [p2wpkh(i) for i in range(1, 6)]
    req = {"candidates": [{"outputs": [{"scriptPubKey": s.hex()}]} for s in scripts]}
    bt = n.rpc("getattestationbatch", req)
    h.check("batch call returns one candidate per request and the template fields", len(bt["candidates"]) == 6 and bt["height"] == n.rpc("getblockcount") + 1 and len(bt["transactions"]) == 3)
    h.check("the batch template reports the same tip as the node", bt["tip"] == n.rpc("getbestblockhash"))

    o = Oracle(n, scripts)
    datas = [bytes.fromhex(x["data"]) for x in bt["transactions"]]
    base = o.base_after_txs(bt["height"], datas)
    wc = bytes.fromhex(bt["witness_commitment_script"])
    for i, (s, c) in enumerate(zip(scripts, bt["candidates"])):
        stripped = build_coinbase({"height": bt["height"], "coinbase_script_sig_prefix": bt["coinbase_script_sig_prefix"], "coinbasevalue": bt["coinbasevalue"],
                                   "coinbase_required_outputs": [{"value": 0, "scriptPubKey": (b"\x6a\x00" + b"\x00" * 33).hex()}, {"value": 0, "scriptPubKey": wc.hex()}]}, s, tx_datas=datas)
        dg = o.candidate_digest(base, bt["height"], stripped["stripped_txid"], bt["coinbasevalue"], s)
        h.check(f"candidate {i}: node attestation equals the independent oracle", bytes.fromhex(c["attestation_script"]) == attestation_script(bt["height"], dg), c["attestation_script"][-12:])
        h.check(f"candidate {i}: coinbase_txid is the txid of the stripped coinbase", bytes.fromhex(c["coinbase_txid"])[::-1] == stripped["stripped_txid"])

    for i in (0, 3):
        head, wit, rest, ntx = build_block(bt, bt["candidates"][i], datas)
        r = proposal(n, block_hex_unsolved(head, wit, rest, ntx))
        h.check(f"candidate {i}: getblocktemplate proposal accepts the assembled block", r is None, str(r))

    # many outputs, tail output (accounting commitment shape), extranonce suffix
    commit = opret(b"PHOT" + b"\x07" * 32)
    split = [{"scriptPubKey": p2wpkh(i).hex(), "value": 1_000_000 * (i + 1)} for i in range(1, 8)] + [{"scriptPubKey": p2wpkh(9).hex()}]
    rich = n.rpc("getattestationbatch", {"candidates": [
        {"outputs": split, "tail_outputs": [{"scriptPubKey": commit.hex()}], "script_sig_suffix": "0102030405"},
        {"outputs": split, "tail_outputs": [{"scriptPubKey": commit.hex()}], "script_sig_suffix": "0102030406"},
    ]})
    a, b = rich["candidates"]
    h.check("variants that differ only in the scriptSig suffix get different attestations", a["attestation_script"] != b["attestation_script"])
    for i, c in enumerate(rich["candidates"]):
        head, wit, rest, ntx = build_block(rich, c, [bytes.fromhex(x["data"]) for x in rich["transactions"]])
        r = proposal(n, block_hex_unsolved(head, wit, rest, ntx))
        h.check(f"multi-output coinbase with commitment and extranonce, variant {i}: accepted by proposal", r is None, str(r))
    cb = bytes.fromhex(a["coinbase_hex"])
    h.check("the returned coinbase carries the commitment as its last output", commit in cb)

    def refused(params, text):
        try:
            n.rpc("getattestationbatch", params); return False
        except RuntimeError as e:
            return text in str(e)
    h.check("outputs above the reward are refused", refused({"candidates": [{"outputs": [{"scriptPubKey": "51", "value": 10**15}]}]}, "exceed"))
    h.check("two outputs without a value are refused", refused({"candidates": [{"outputs": [{"scriptPubKey": "51"}, {"scriptPubKey": "51"}]}]}, "at most one"))
    h.check("an output shaped like an attestation is refused", refused({"candidates": [{"outputs": [{"scriptPubKey": "51"}], "tail_outputs": [{"scriptPubKey": attestation_script(bt['height'], b'\x01' * 32).hex()}]}]}, "attestation"))
    h.check("an oversized scriptSig is refused", refused({"candidates": [{"outputs": [{"scriptPubKey": "51"}], "script_sig_suffix": "00" * 120}]}, "scriptSig"))
    h.check("an empty candidate list is refused", refused({"candidates": []}, "1 to 4096"))
    h.check("too many candidates are refused", refused({"candidates": [{"outputs": [{"scriptPubKey": "51"}]}] * 4097}, "1 to 4096"))

    # block built from a batch candidate is accepted by submitblock
    bt2 = n.rpc("getattestationbatch", {"candidates": [{"outputs": [{"scriptPubKey": p2wpkh(1).hex()}]}]})
    head, wit, rest, ntx = build_block(bt2, bt2["candidates"][0], [bytes.fromhex(x["data"]) for x in bt2["transactions"]])
    target = int(bt2["target"], 16)
    nonce = 0
    while int.from_bytes(sha256d(head + struct.pack("<I", nonce))[::-1], "big") > target: nonce += 1
    r = n.rpc("submitblock", (head + struct.pack("<I", nonce) + varint(ntx) + wit + rest).hex())
    h.check("a block assembled from a batch candidate is accepted by submitblock and pays the payout script", r is None and n.rpc("getblock", n.rpc("getbestblockhash"), 2)["tx"][0]["vout"][0]["scriptPubKey"]["hex"] == p2wpkh(1).hex(), str(r))

    # timing with many candidates
    for count in (100, 1000, 4000):
        req = {"candidates": [{"outputs": [{"scriptPubKey": p2wpkh(1000 + i).hex()}]} for i in range(count)]}
        t0 = time.perf_counter(); big = n.rpc("getattestationbatch", req); dt = (time.perf_counter() - t0) * 1000
        print(f"  info batch of {count}: {dt:.0f} ms total ({dt / count:.3f} ms per candidate, incl. template build and JSON)")
        h.check(f"batch of {count} returns {count} distinct attestations", len({c['attestation_script'] for c in big['candidates']}) == count)
    # proposal-validate a sample of the big batch
    sample = [big["candidates"][i] for i in (0, 1999, 3999)]
    ok = all(proposal(n, block_hex_unsolved(*build_block(big, c, [bytes.fromhex(x["data"]) for x in big["transactions"]]))) is None for c in sample)
    h.check("sampled candidates of the 4000 batch are accepted by proposal", ok)
