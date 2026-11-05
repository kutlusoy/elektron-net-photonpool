import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request, urllib.error
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

# What the public listener shows of the engine: the numbers and a sanitized event feed, nothing that identifies a miner or the node.
# The operator listener serves the same public API (so that one page works on both), plus the admin API.
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PUB, STRATUM = 18981, 18982, 18333
ACCOUNT = encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"miner-account").digest()[:20]))
def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]

def write_cfg(path):
    addrs = [encode_segwit_address("bcrt", 0, list(prog(i))) for i in range(1, 5)]
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 20
[pool]
network = "regtest"
payout_addresses = {json.dumps(addrs)}
refresh_secs = 0
[stratum]
listen = "127.0.0.1:{STRATUM}"
initial_difficulty = 0.001
[public]
enabled = true
listen = "127.0.0.1:{PUB}"
[ledger]
path = "{path}.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def raw(port, path):
    try:
        r = urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5); return r.status, r.read().decode()
    except urllib.error.HTTPError as e: return e.code, e.read().decode()

def get(port, path): return json.loads(raw(port, path)[1])

async def main(h):
    for _ in range(100):
        try:
            if get(API, "/api/v1/summary")["alloc"]["candidates"] == 4: break
        except Exception: pass
        await asyncio.sleep(0.1)
    c = await Client().open()
    r = await c.request("mining.configure", [["version-rolling"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}])
    c.mask = int(r["result"]["version-rolling.mask"], 16)
    await c.request("mining.subscribe", ["bitaxe/BM1370/v2.9.0"])
    await c.request("mining.authorize", [f"{ACCOUNT}.secretrig", "x"])
    job = await c.wait_job()
    m, bhash = await mine_and_submit(c, f"{ACCOUNT}.secretrig", job)       # on regtest a share is a block: "BLOCK FOUND" is logged
    await asyncio.sleep(2.0)

    ev = get(PUB, "/public/v1/events?since=0")
    texts = [e["msg"] for e in ev["events"]]
    h.check("the public feed has epoch lines", any(t.startswith("epoch ") and "candidates ready in" in t for t in texts), str(texts[:2]))
    blk = [t for t in texts if t.startswith("BLOCK FOUND")]
    h.check("the found block is in the public feed with a masked finder", len(blk) == 1 and ACCOUNT[:6] + ".." + ACCOUNT[-4:] in blk[0], str(blk))
    body = raw(PUB, "/public/v1/events?since=0")[1] + raw(PUB, "/public/v1/engine")[1] + raw(PUB, "/public/v1/devices")[1] + raw(PUB, "/public/v1/pool")[1] + raw(PUB, "/public/v1/blocks")[1]
    h.check("no worker name and no full address anywhere in the public data", "secretrig" not in body and ACCOUNT not in body)
    h.check("no share or job lines and no error text in the public feed", all(e["lvl"] in ("info", "blk", "warn") for e in ev["events"]) and not any("share" in t.lower() for t in texts))
    last = ev["last"]
    h.check("asking for events after the last one gives none", get(PUB, f"/public/v1/events?since={last}")["events"] == [])

    eng = get(PUB, "/public/v1/engine")
    h.check("the engine numbers have the shape of the admin summary", all(k in eng for k in ("node", "hashrate", "miners", "shares", "alloc")) and "p50" in eng["node"] and "unitsPerSec" in eng["alloc"], str(list(eng)))
    h.check("the engine data says nothing about the Photon listener", eng["photon"] == {"enabled": False}, str(eng["photon"]))
    h.check("the public engine data has nothing about the node's address or credentials", "127.0.0.1" not in json.dumps(eng) and ".cookie" not in json.dumps(eng))

    # the operator listener serves the public API as well, and the admin API stays on it only
    h.check("the operator listener serves the public API too", get(API, "/public/v1/engine")["alloc"]["candidates"] == 4 and "events" in get(API, "/public/v1/events?since=0"))
    h.check("the admin API is not served on the public listener", raw(PUB, "/api/v1/summary")[0] == 404 and raw(PUB, "/api/v1/miners")[0] == 404 and raw(PUB, "/api/v1/events")[0] == 404)
    for path in ("/api/v1/ledger", "/api/v1/account/" + ACCOUNT):
        h.check(f"{path} is not on the public listener", raw(PUB, path)[0] == 404)
    c.w.close()

with Harness("step26") as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
