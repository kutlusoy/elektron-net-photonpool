"""A full block with a large coinbase must stay within the block weight limit: the batch RPC reserves room for the largest coinbase of its request.
The limit is lowered with -blockmaxweight so that a full block is cheap to build."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import struct
from decimal import Decimal
from photon_testlib import *
from photon_testlib import block_from_batch

LIMIT = 100_000

def fanout(prev, value, outputs=100, fee=5000):
    each = (value - fee) // outputs
    raw = struct.pack("<i", 2) + varint(1) + prev + struct.pack("<I", 0) + b"\x00" + struct.pack("<I", 0xfffffffd) + varint(outputs) + b"".join(txout(each, b"\x51") for _ in range(outputs)) + struct.pack("<I", 0)
    return raw, each

def wrap(cb, reserved):
    return cb[:4] + b"\x00\x01" + cb[4:-4] + varint(1) + varint(32) + reserved + cb[-4:]

with Harness("step22", blocks=130, extra=["-acceptnonstdtxn=1", f"-blockmaxweight={LIMIT}"]) as h:
    n = h.node
    outs = []
    for height in range(1, 7):
        cb = n.rpc("getblock", n.rpc("getblockhash", height), 2)["tx"][0]
        raw, each = fanout(bytes.fromhex(cb["txid"])[::-1], int(Decimal(str(cb["vout"][0]["value"])) * LEPTON))
        n.rpc("sendrawtransaction", raw.hex())
        outs += [(sha256d(raw), i, each) for i in range(100)]
    while n.rpc("getmempoolinfo")["size"] > 0:
        n.rpc("generatetodescriptor", 1, "raw(51)")
    for txid, i, v in outs:
        raw, _ = make_spend(txid, i, v, outputs=2)
        n.rpc("sendrawtransaction", raw.hex())
    print(f"  info mempool has {n.rpc('getmempoolinfo')['size']} transactions, the weight limit is {LIMIT}")

    def weight_for(count_outputs):
        cand = {"outputs": [{"scriptPubKey": p2wpkh_script(i).hex(), "value": 1000} for i in range(count_outputs)]}
        bt = n.rpc("getattestationbatch", {"candidates": [cand]})
        datas = [bytes.fromhex(x["data"]) for x in bt["transactions"]]
        cbn = bytes.fromhex(bt["candidates"][0]["coinbase_hex"])
        wit = wrap(cbn, bytes.fromhex(bt.get("coinbase_witness", "00" * 32))[::-1])
        # the transactions of this test carry no witness: weight = 4 * size
        weight = 4 * (80 + len(varint(1 + len(datas)))) + 3 * len(cbn) + len(wit) + 4 * sum(len(d) for d in datas)
        err = proposal(n, block_hex_unsolved(*block_from_batch(bt, bt["candidates"][0], datas)))
        return len(datas), len(cbn), weight, err

    for count in (1, 128, 200):
        ntx, cblen, weight, err = weight_for(count)
        h.check(f"coinbase with {count} outputs ({cblen} B): block of {ntx + 1} transactions is valid and weighs {weight} <= {LIMIT}", err is None and weight <= LIMIT, str(err))
        h.check(f"the block with {count} outputs is nearly full ({weight} > {LIMIT - 12000})", weight > LIMIT - 12000)
