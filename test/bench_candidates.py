import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, subprocess
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
COUNTS = [int(x) for x in os.environ.get("COUNTS", "100,430,2000").split(",")]

def cfg(path, count, backend):
    addrs = [encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"bench-%d" % i).digest()[:20])) for i in range(count)]
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
[pool]
network = "regtest"
backend = "{backend}"
payout_addresses = {json.dumps(addrs)}
[ledger]
path = "{path}.db"
''')

with Harness("bench-candidates") as h:
    base = h.node.datadir + "-cfg"
    for count in COUNTS:
        for backend in ("gbt", "batch"):
            path = f"{base}-{count}-{backend}.toml"; cfg(path, count, backend)
            r = subprocess.run([PHOTOND, "-c", path, "check"], capture_output=True, text=True, timeout=300)
            if r.returncode != 0:
                print(f"  {count:>5} addresses, {backend:>5}: FAILED {r.stderr.strip()[-150:]}"); continue
            rep = json.loads(r.stdout)
            print(f"  {count:>5} addresses, {backend:>5}: fetch {rep['fetch_ms']:>5} ms, {rep['candidates']} candidates, all validated by the node, rejected {rep['rejected']}")
