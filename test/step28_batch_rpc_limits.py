import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, threading, time
from photon_testlib import *

# Hardening of getattestationbatch (review of the node patch): totals over the whole request are budgeted, values are summed overflow-safe, a coinbase of more
# than half a block is refused, a third parallel call is refused at once, and the node keeps validating blocks while a heavy request is being answered.
# Needs an elektrond built with the Photon node patch (ELEKTROND_BATCH).

def refused(n, req, needle):
    try: n.rpc("getattestationbatch", req)
    except RuntimeError as e: return needle in str(e)
    return False

with Harness("step28", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    ok_script = p2wpkh_script(1).hex()
    one = lambda outs, **k: {"candidates": [dict({"outputs": outs}, **k)]}
    h.check("a normal request works", len(n.rpc("getattestationbatch", one([{"scriptPubKey": ok_script}]))["candidates"]) == 1)
    reward = n.rpc("getattestationbatch", one([{"scriptPubKey": ok_script}]))["coinbasevalue"]
    h.check("values that add up above the reward are refused", refused(n, one([{"scriptPubKey": ok_script, "value": reward}, {"scriptPubKey": ok_script, "value": 1}]), "exceed the block reward"))
    h.check("values near MAX_MONEY cannot overflow the sum, they are refused", refused(n, {"candidates": [{"outputs": [{"scriptPubKey": ok_script, "value": 2100000000000000}] * 500 + [{"scriptPubKey": ok_script}]}]}, "exceed the block reward"))
    h.check("a negative value is refused", refused(n, one([{"scriptPubKey": ok_script, "value": -1}]), "out of range"))
    # total budget: 401 candidates with 1000 outputs each is 401000 outputs
    big = {"candidates": [{"outputs": [{"scriptPubKey": "51", "value": 0}] * 1000} for _ in range(401)]}
    h.check("a request above the output budget is refused before any work", refused(n, {"candidates": big["candidates"]}, "budget"))
    # scripts: 900 outputs of 10000 bytes (9 MB) exceed the 8 MiB script budget
    fat = "51" * 10000
    h.check("a request above the script byte budget is refused", refused(n, {"candidates": [{"outputs": [{"scriptPubKey": fat, "value": 0}] * 900}]}, "budget"))
    # a coinbase that needs more than half of the block weight (about 500 KB): 60 outputs of 10000 bytes per candidate is 600 KB
    h.check("a coinbase of more than half a block is refused", refused(n, {"candidates": [{"outputs": [{"scriptPubKey": fat, "value": 0}] * 60}]}, "half of the block weight"))
    # parallel calls: two may run, a third fails at once while they hold their slots
    heavy = {"candidates": [{"outputs": [{"scriptPubKey": "51", "value": 0}] * 1000} for _ in range(300)]}
    res = []
    def call():
        t0 = time.time()
        try: n.rpc("getattestationbatch", heavy); res.append(("ok", time.time() - t0))
        except RuntimeError as e: res.append(("busy" if "busy" in str(e) else "err:" + str(e)[:60], time.time() - t0))
    ths = [threading.Thread(target=call) for _ in range(6)]
    [t.start() for t in ths]; [t.join() for t in ths]
    kinds = sorted(k for k, _ in res)
    h.check("parallel heavy calls: some are served, the surplus is refused as busy (nothing else goes wrong)", "ok" in kinds and all(k in ("ok", "busy") for k in kinds), str(kinds))
    # the node keeps answering and validating while heavy calls run
    done = threading.Event()
    def hammer():
        for _ in range(3):
            try: n.rpc("getattestationbatch", heavy)
            except RuntimeError: pass
        done.set()
    t = threading.Thread(target=hammer); t.start()
    worst = 0.0
    while not done.is_set():
        t0 = time.time(); n.rpc("getblockchaininfo"); worst = max(worst, time.time() - t0)
    t.join()
    h.check("getblockchaininfo stays responsive during heavy batch calls", worst < 2.0, f"worst {worst * 1000:.0f} ms")
    before = n.rpc("getblockcount")
    n.rpc("generatetodescriptor", 3, "raw(51)")
    h.check("blocks are still produced and validated afterwards", n.rpc("getblockcount") == before + 3)
