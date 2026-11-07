import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from photon_testlib import *

# A young chain has no fee estimate: sendmany without a fee rate fails ("Fee estimation failed. Fallbackfee is disabled"), with the
# fixed fee rate the pool passes (payout.fee_rate, PAYOUT_FEE_RATE in the stack, default 2 sat/vB) it works.
with Harness("step35", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    n.rpc("createwallet", "w")
    a = n.rpc("getnewaddress", wallet="w")
    n.rpc("generatetoaddress", 101, a, wallet="w")
    dest = n.rpc("getnewaddress", wallet="w")
    try:
        n.rpc("sendmany", "", {dest: 1.0}, 1, "c1", wallet="w"); failed = False
    except Exception as e: failed = "Fee estimation failed" in str(e)
    h.check("without a fee rate the node cannot send on a chain without estimates", failed)
    try: txid = n.rpc("sendmany", "", {dest: 1.0}, 1, "c2", [], None, None, None, 2, wallet="w")
    except Exception as e: txid = None
    h.check("with the fixed fee rate 2 sat/vB the payout call works", bool(txid) and len(txid) == 64, str(txid))
