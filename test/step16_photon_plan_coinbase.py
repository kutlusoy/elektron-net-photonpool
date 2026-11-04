"""Photon plan end to end: the Rust plan derivation produces a coinbase layout that the (patched) node attests,
the node's validator accepts, the Rust checker accepts, and that pays the miners as planned on chain."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, subprocess
from decimal import Decimal
from photon_testlib import *

PHOTOND = os.environ.get("PHOTOND", os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "target", "release", "photond"))
FEE_SCRIPT = p2wpkh_script(900)
RULES = {"n_max": 200, "fee_ppb": 10_000_000, "fee_script": FEE_SCRIPT.hex()}

def cli(cmd, payload):
    r = subprocess.run([PHOTOND, cmd], input=json.dumps(payload), capture_output=True, text=True)
    return r.returncode, json.loads(r.stdout)

with Harness("step16", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    cbtx = n.rpc("getblock", n.rpc("getblockhash", 1), 2)["tx"][0]
    raw, _ = make_spend(bytes.fromhex(cbtx["txid"])[::-1], 0, int(Decimal(str(cbtx["vout"][0]["value"])) * LEPTON), outputs=2)
    n.rpc("sendrawtransaction", raw.hex())

    credits = [{"script": p2wpkh_script(i).hex(), "credit": 1000 + 137 * i * i} for i in range(1, 41)] + [{"script": p2wpkh_script(500).hex(), "credit": 3}]
    gbt = n.gbt()
    reward, tip, height = gbt["coinbasevalue"], n.rpc("getbestblockhash"), gbt["height"]
    base = {"tip": tip, "height": height, "reward": reward, "rules": RULES, "credits": credits}

    variants = []
    for v in range(4):
        rc, out = cli("photon-plan", {**base, "suffix": bytes([1, 2, 3, v]).hex()})
        h.check(f"variant {v}: the plan and the node candidate are derived", rc == 0, str(out)[:200])
        variants.append(out)
    plan = variants[0]["plan"]
    h.check("the plan is identical for all variants", all(x["plan"] == plan for x in variants))
    h.check("the smallest credit (3 of ~3 million) is paid too, nobody is left out", len(plan["outputs"]) == 41 and p2wpkh_script(500).hex() in [o["script"] for o in plan["outputs"]])

    bt = n.rpc("getattestationbatch", {"candidates": [x["candidate"] for x in variants]})
    h.check("the node attests all four variants", len(bt["candidates"]) == 4 and len({c["attestation_script"] for c in bt["candidates"]}) == 4)
    datas = [bytes.fromhex(x["data"]) for x in bt["transactions"]]
    for v, c in enumerate(bt["candidates"]):
        rc, out = cli("photon-check", {"plan": plan, "coinbase_hex": c["coinbase_hex"], "max_reward": bt["coinbasevalue"]})
        h.check(f"variant {v}: the Rust checker accepts the node's coinbase for the plan", rc == 0 and out["reward"] == bt["coinbasevalue"], str(out))
        err = proposal(n, block_hex_unsolved(*block_from_batch(bt, c, datas)))
        h.check(f"variant {v}: the node's validator accepts the block", err is None, str(err))

    # negative: the checker refuses a coinbase for a different plan, a lower bound, and a tampered plan
    other = dict(plan); other["fee_ppb"] = 0
    rc, out = cli("photon-check", {"plan": other, "coinbase_hex": bt["candidates"][0]["coinbase_hex"], "max_reward": bt["coinbasevalue"]})
    h.check("a plan with other terms is refused", rc != 0)
    rc, out = cli("photon-check", {"plan": plan, "coinbase_hex": bt["candidates"][0]["coinbase_hex"], "max_reward": bt["coinbasevalue"] - 1})
    h.check("a reward above the bound is refused", rc != 0)
    swapped = json.loads(json.dumps(plan)); swapped["outputs"][0]["script"] = p2wpkh_script(777).hex()
    rc, out = cli("photon-check", {"plan": swapped, "coinbase_hex": bt["candidates"][0]["coinbase_hex"], "max_reward": bt["coinbasevalue"]})
    h.check("a plan whose hash does not match, or whose script differs from the coinbase, is refused", rc != 0)

    # more credits than outputs: the smaller ones take turns, the plan stays within the limit and the node accepts the coinbase
    many = [{"script": p2wpkh_script(1000 + i).hex(), "credit": 1000 + 3 * i} for i in range(300)]
    rc, out = cli("photon-plan", {**base, "rules": {**RULES, "n_max": 40}, "credits": many, "suffix": "09"})
    h.check("300 credits with a limit of 40 outputs give a plan of at most 40 outputs", rc == 0 and 0 < len(out["plan"]["outputs"]) <= 40, str(rc))
    bt3 = n.rpc("getattestationbatch", {"candidates": [out["candidate"]]})
    err = proposal(n, block_hex_unsolved(*block_from_batch(bt3, bt3["candidates"][0], [bytes.fromhex(x["data"]) for x in bt3["transactions"]])))
    h.check("the node validates the coinbase of the turn-taking plan", err is None, str(err))
    rc, chk = cli("photon-check", {"plan": out["plan"], "coinbase_hex": bt3["candidates"][0]["coinbase_hex"], "max_reward": bt3["coinbasevalue"]})
    h.check("the Rust checker accepts it as well", rc == 0, str(chk))

    # on chain
    r = n.rpc("submitblock", solve_parts(bt, block_from_batch(bt, bt["candidates"][2], datas)))
    h.check("the block is accepted by submitblock", r is None, str(r))
    blk = n.rpc("getblock", n.rpc("getbestblockhash"), 2)
    outs = blk["vout"] if "vout" in blk else blk["tx"][0]["vout"]
    paid = {o["scriptPubKey"]["hex"]: int(Decimal(str(o["value"])) * LEPTON) for o in outs if o["value"] > 0}
    total = sum(paid.values())
    h.check("the whole reward is paid out on chain", total == reward, f"{total} vs {reward}")
    ok = all(paid[o["script"]] == reward * o["ppb"] // 1_000_000_000 for o in plan["outputs"])
    h.check("every miner receives floor(reward * ppb / 1e9)", ok)
    h.check("the pool fee output holds the remainder, at least the agreed 1 %", paid[FEE_SCRIPT.hex()] >= reward // 100)
    h.check("the smallest miner receives its (tiny) part on chain", paid.get(p2wpkh_script(500).hex(), 0) > 0, str(paid.get(p2wpkh_script(500).hex())))
    opret = [o["scriptPubKey"]["hex"] for o in outs if o["scriptPubKey"]["hex"].startswith("6a24" + b"PHOT".hex())]
    h.check("the accounting root is committed on chain", opret == ["6a24" + b"PHOT".hex() + plan["accounting_root"]], str(opret))
