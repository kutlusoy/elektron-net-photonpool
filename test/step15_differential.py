"""Differential test of the getattestationbatch node patch.

Every candidate of every batch is checked against three independent references:
  1. the Python oracle (own MuHash over the UTXO set, own coinbase serialisation),
  2. the node's own validator (getblocktemplate proposal on the assembled block),
  3. the unpatched code path (getblocktemplate with coinbaseaddress) for single-output candidates.
Also: order independence, duplicates, no side effects on chainstate/mempool, reorgs, and the activation heights.
Regtest: MuHash attestation from height 50, intra-block fix from height 110.
SEED, ROUNDS and MAXCAND override the defaults.
"""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, random, struct, time
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address

SEED = int(os.environ.get("SEED", "1"))
ROUNDS = int(os.environ.get("ROUNDS", "40"))
MAXCAND = int(os.environ.get("MAXCAND", "48"))
MUHASH_H, FIX_H = 50, 110
rnd = random.Random(SEED)
POOL = [p2wpkh_script(i) for i in range(40)]
SCRIPTS = [b"\x51"] + POOL
def addr(script): return encode_segwit_address("bcrt", 0, list(script[2:]))

def tx_raw(inputs, outs):
    vin = b"".join(t + struct.pack("<I", n) + b"\x00" + struct.pack("<I", 0xfffffffd) for t, n in inputs)
    return struct.pack("<i", 2) + varint(len(inputs)) + vin + varint(len(outs)) + b"".join(txout(v, s) for v, s in outs) + struct.pack("<I", 0)

def wrap_witness(cb_nowit, reserved):
    return cb_nowit[:4] + b"\x00\x01" + cb_nowit[4:-4] + varint(1) + varint(32) + reserved + cb_nowit[-4:]

def block_from(bt, cand, datas):
    cb = bytes.fromhex(cand["coinbase_hex"])
    wit = wrap_witness(cb, bytes.fromhex(bt.get("coinbase_witness", "00" * 32))[::-1])
    leaves = [sha256d(cb)] + [sha256d(d) for d in datas]
    while len(leaves) > 1:
        if len(leaves) % 2: leaves.append(leaves[-1])
        leaves = [sha256d(leaves[i] + leaves[i + 1]) for i in range(0, len(leaves), 2)]
    head = struct.pack("<i", bt["version"]) + bytes.fromhex(bt["tip"])[::-1] + leaves[0] + struct.pack("<I", bt["curtime"]) + bytes.fromhex(bt["bits"])[::-1]
    return head, wit, b"".join(datas), 1 + len(datas)

def solved(bt, parts):
    head, wit, rest, ntx = parts
    target = int(bt["target"], 16); nonce = 0
    while int.from_bytes(sha256d(head + struct.pack("<I", nonce))[::-1], "big") > target: nonce += 1
    return (head + struct.pack("<I", nonce) + varint(ntx) + wit + rest).hex()

class Pool:
    """Tracks spendable raw(51) outputs for the random mempool."""
    def __init__(self, n): self.n, self.used, self.free = n, set(), []
    def mature(self):
        res = self.n.rpc("scantxoutset", "start", ["raw(51)"])
        tip = self.n.rpc("getblockcount")
        out = []
        for u in res["unspents"]:
            key = (bytes.fromhex(u["txid"])[::-1], u["vout"])
            if u.get("coinbase") and tip - u["height"] + 1 < 100: continue
            if key in self.used: continue
            out.append((key[0], key[1], int(Decimal(u["amount"]) * LEPTON)))
        return out
    def fill(self, count, chains=True):
        sent, raws = 0, []
        avail = self.mature() + self.free
        self.free = []
        rnd.shuffle(avail)
        for _ in range(count):
            if not avail: break
            k = min(len(avail), rnd.choice([1, 1, 2]))
            ins = [avail.pop() for _ in range(k)]
            total = sum(v for _, _, v in ins)
            nout = rnd.randint(2, 6); fee = 2000
            each = (total - fee) // nout
            outs = []
            for _ in range(nout):
                kind = rnd.random()
                if kind < 0.55: outs.append((each, b"\x51"))
                elif kind < 0.9: outs.append((each, rnd.choice(POOL)))
                else: outs.append((0, op_return(bytes(rnd.getrandbits(8) for _ in range(rnd.randint(0, 30))))))
            raw = tx_raw([(t, n) for t, n, _ in ins], outs)
            try: self.n.rpc("sendrawtransaction", raw.hex())
            except RuntimeError: continue
            sent += 1; txid = sha256d(raw)
            for t, nn, _ in ins: self.used.add((t, nn))
            for i, (v, s) in enumerate(outs):
                if chains and s == b"\x51" and v > 20000: avail.append((txid, i, v))
        self.free = avail
        return sent

def make_candidates(reward, count):
    cands = []
    for _ in range(count):
        k = rnd.choice([1, 1, 1, 2, 3, 5, 8, 12])
        scripts = [rnd.choice(POOL) for _ in range(k)]
        exact = rnd.random() < 0.4 and k > 1
        shares = [rnd.random() + 0.05 for _ in range(k)]
        budget = reward if exact else reward // 2
        vals = [int(budget * s / sum(shares)) for s in shares]
        outs = [{"scriptPubKey": s.hex(), "value": v} for s, v in zip(scripts, vals)]
        if exact: outs[-1]["value"] += reward - sum(o["value"] for o in outs)
        elif rnd.random() < 0.7: del outs[-1]["value"]
        c = {"outputs": outs}
        tails = []
        for _ in range(rnd.choice([0, 0, 1, 2])):
            if rnd.random() < 0.8: tails.append({"scriptPubKey": op_return(bytes(rnd.getrandbits(8) for _ in range(rnd.randint(0, 40)))).hex()})
            else: tails.append({"scriptPubKey": rnd.choice(POOL).hex()})   # zero-value spendable tail output
        if tails: c["tail_outputs"] = tails
        if rnd.random() < 0.6: c["script_sig_suffix"] = bytes(rnd.getrandbits(8) for _ in range(rnd.randint(1, 30))).hex()
        cands.append(c)
    return cands

def local_coinbase(bt, c, attestation, reward):
    """Own serialisation of the final coinbase and the stripped txid for a request candidate."""
    outs = []
    left = reward
    for o in c["outputs"]:
        if "value" in o: left -= o["value"]
    for o in c["outputs"]:
        outs.append((o["value"] if "value" in o else left, bytes.fromhex(o["scriptPubKey"])))
    tail = [(o.get("value", 0), bytes.fromhex(o["scriptPubKey"])) for o in c.get("tail_outputs", [])]
    wc = bytes.fromhex(bt["witness_commitment_script"])
    sig = bytes.fromhex(bt["coinbase_script_sig_prefix"]) + bytes.fromhex(c.get("script_sig_suffix", ""))
    vin = b"\x00" * 32 + b"\xff\xff\xff\xff" + varint(len(sig)) + sig + struct.pack("<I", 0xfffffffe)
    def ser(o): return struct.pack("<i", 2) + varint(1) + vin + varint(len(o)) + b"".join(txout(v, s) for v, s in o) + struct.pack("<I", bt["height"] - 1)
    return ser(outs + [(0, attestation), (0, wc)] + tail), sha256d(ser(outs + [(0, wc)] + tail)), outs, tail

def state(n):
    return (n.rpc("getbestblockhash"), sorted(n.rpc("getrawmempool")), n.rpc("gettxoutsetinfo", "muhash")["muhash"])

with Harness("step15", blocks=45, extra=["-acceptnonstdtxn=1", "-limitancestorcount=50", "-limitdescendantcount=100"]) as h:
    n = h.node
    pool = Pool(n)
    print(f"  info seed {SEED}, rounds {ROUNDS}")

    # activation: below the MuHash height the call is refused
    try:
        n.rpc("getattestationbatch", {"candidates": [{"outputs": [{"scriptPubKey": POOL[0].hex()}]}]})
        refused = False
    except RuntimeError as e: refused = "unsupported-height" in str(e)
    h.check(f"height {n.rpc('getblockcount') + 1} (below the MuHash activation): refused", refused)
    n.rpc("generatetodescriptor", 100 - n.rpc("getblockcount"), "raw(51)")   # first spendable coinbase at height 101, still before the fix at 110

    total_cands = checked = 0
    for r in range(ROUNDS):
        height = n.rpc("getblockcount") + 1
        phase = "pre-fix" if height < FIX_H else "post-fix"
        sent = pool.fill(rnd.choice([0, 3, 10, 25]), chains=height >= FIX_H)
        before = state(n)
        try: gbt = n.gbt()
        except RuntimeError as e:
            print(f"  info round {r} h={height} ({phase}): the node's own getblocktemplate fails: {e}")
            try:
                n.rpc("getattestationbatch", {"candidates": [{"outputs": [{"scriptPubKey": POOL[0].hex()}]}]}); same = False
            except RuntimeError: same = True
            h.check("  the batch call fails in the same state", same)
            raise
        reward = gbt["coinbasevalue"]
        cands = make_candidates(reward, rnd.randint(1, MAXCAND))
        # a duplicated candidate and one that only differs in the suffix
        cands.append(json.loads(json.dumps(cands[0])))
        twin = json.loads(json.dumps(cands[0])); twin["script_sig_suffix"] = "aa" + twin.get("script_sig_suffix", ""); cands.append(twin)
        bt = n.rpc("getattestationbatch", {"candidates": cands})
        h.check(f"round {r} h={height} ({phase}): {sent} mempool txs, {len(cands)} candidates, template matches the node's", bt["height"] == height and bt["tip"] == before[0] and bt["coinbasevalue"] == reward
                and [x["txid"] for x in bt["transactions"]] == [x["txid"] for x in gbt["transactions"]])
        h.check("  no side effects (tip, mempool, UTXO MuHash unchanged)", state(n) == before)
        datas = [bytes.fromhex(x["data"]) for x in bt["transactions"]]

        # order independence
        order = list(range(len(cands))); rnd.shuffle(order)
        shuf = n.rpc("getattestationbatch", {"candidates": [cands[i] for i in order]})
        same = all(shuf["candidates"][pos] == bt["candidates"][i] for pos, i in enumerate(order))
        h.check("  shuffled request gives identical per-candidate results", same)
        h.check("  duplicate candidates give identical results", bt["candidates"][0] == bt["candidates"][-2])
        h.check("  a one-byte suffix change changes the attestation", bt["candidates"][0]["attestation_script"] != bt["candidates"][-1]["attestation_script"])

        oracle, base = None, None
        if height >= FIX_H:
            oracle = Oracle(n, SCRIPTS)
            base = oracle.base_after_txs(height, datas)
        bad = []
        for i, (c, res) in enumerate(zip(cands, bt["candidates"])):
            total_cands += 1
            att = bytes.fromhex(res["attestation_script"])
            nowit, stripped_txid, outs, tail = local_coinbase(bt, c, att, reward)
            if bytes.fromhex(res["coinbase_hex"]) != nowit: bad.append((i, "coinbase_hex differs from the local serialisation")); continue
            if bytes.fromhex(res["coinbase_txid"])[::-1] != stripped_txid: bad.append((i, "coinbase_txid differs")); continue
            if oracle:
                extra = [(len(outs) + 1 + j, (v, s)) for j, (v, s) in enumerate(tail) if not is_unspendable(s)]
                payouts = [(v, s) for v, s in outs]
                num = base
                for idx, (v, s) in enumerate(payouts):
                    num = num * coin_hash_num(coin_ser(stripped_txid, idx, height, True, v, s)) % MOD
                for idx, (v, s) in extra:
                    num = num * coin_hash_num(coin_ser(stripped_txid, idx, height, True, v, s)) % MOD
                if attestation_script(height, digest_of(num)) != att: bad.append((i, "oracle digest differs"))
                    
            err = proposal(n, block_hex_unsolved(*block_from(bt, res, datas)))
            if err is not None: bad.append((i, f"proposal rejected: {err}"))
            checked += 1
        h.check(f"  all {len(cands)} candidates agree with the {'oracle, ' if oracle else ''}local serialisation and the validator", not bad, str(bad[:4]))

        # unpatched path: single plain output, no suffix, no tail
        plain = [c for c in cands if len(c["outputs"]) == 1 and "tail_outputs" not in c and "script_sig_suffix" not in c and "value" not in c["outputs"][0]]
        for c in plain[:3]:
            s = bytes.fromhex(c["outputs"][0]["scriptPubKey"])
            g = n.rpc("getblocktemplate", {"rules": ["segwit"], "coinbaseaddress": addr(s)})
            node_att = next(bytes.fromhex(o["scriptPubKey"]) for o in g["coinbase_required_outputs"] if not o["scriptPubKey"].startswith(WC_PREFIX.hex()))
            mine = bt["candidates"][cands.index(c)]
            h.check("  plain candidate: attestation equals getblocktemplate with coinbaseaddress", node_att == bytes.fromhex(mine["attestation_script"]))

        # advance the chain with a random candidate; this block is then part of the next oracle state
        pick = rnd.randrange(len(cands))
        r_sub = n.rpc("submitblock", solved(bt, block_from(bt, bt["candidates"][pick], datas)))
        h.check(f"  block from candidate {pick} accepted by submitblock", r_sub is None, str(r_sub))
        if r % 5 == 3 and n.rpc("getblockcount") > 120:
            tip = n.rpc("getbestblockhash"); prev = n.rpc("getblock", tip)["previousblockhash"]
            n.rpc("invalidateblock", prev)
            bt2 = n.rpc("getattestationbatch", {"candidates": [{"outputs": [{"scriptPubKey": POOL[1].hex()}]}]})
            err = proposal(n, block_hex_unsolved(*block_from(bt2, bt2["candidates"][0], [bytes.fromhex(x["data"]) for x in bt2["transactions"]])))
            h.check("  candidate on the reorged tip accepted by the validator", err is None, str(err))
            n.rpc("reconsiderblock", prev)
            h.check("  chain restored after reconsiderblock", n.rpc("getbestblockhash") == tip)
    print(f"  info {total_cands} candidates compared, {checked} validated by the node")
