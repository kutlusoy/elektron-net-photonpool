import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

# Device list by user agent (name, link, open source flag, working connections) and the stored pool history that new visitors see.
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
stats_keep_days = 7
device_history_minutes = 1
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def get(port, path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=5).read())

async def connect(agent, worker):
    c = await Client().open()
    r = await c.request("mining.configure", [["version-rolling"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}])
    c.mask = int(r["result"]["version-rolling.mask"], 16)
    await c.request("mining.subscribe", [agent])
    await c.request("mining.authorize", [f"{ACCOUNT}.{worker}", "x"])
    await c.wait_job()
    return c

async def main(h):
    for _ in range(100):
        try:
            if get(API, "/api/v1/summary")["alloc"]["candidates"] == 4: break
        except Exception: pass
        await asyncio.sleep(0.1)
    cs = [await connect("bitaxe/BM1370/v2.9.0", "a1"), await connect("bitaxe/BM1370/v2.9.0", "a2"), await connect("NerdMiner/1.0", "n1"), await connect("MyOwnMiner/3 (x)", "o1")]
    # a valid share (on regtest it is a block) gives the first device a best difficulty
    c0 = cs[0]; job = c0.job
    m, _ = await mine_and_submit(c0, f"{ACCOUNT}.a1", job)
    await asyncio.sleep(1.5)
    for port, path in [(API, "/api/v1/devices"), (PUB, "/public/v1/devices")]:
        d = get(port, path)
        names = {x["name"]: x for x in d["devices"]}
        h.check(f"{path}: the devices are listed by name ({sorted(names)})", {"Bitaxe", "NerdMiner", "MyOwnMiner"} <= set(names))
        h.check(f"{path}: two Bitaxes are working, with a link and the open source star", names["Bitaxe"]["working"] == 2 and names["Bitaxe"]["url"] == "https://github.com/skot/bitaxe/" and names["Bitaxe"]["openSource"] is True, str(names["Bitaxe"]))
        h.check(f"{path}: an unknown agent has no link", names["MyOwnMiner"]["url"] is None and names["MyOwnMiner"]["openSource"] is False)
    names = {x["name"]: x for x in get(API, "/api/v1/devices")["devices"]}
    h.check("the Bitaxe has a best share difficulty after its share was accepted", names["Bitaxe"]["best"] > 0 and names["NerdMiner"]["best"] == 0, str(names["Bitaxe"]["best"]))
    miners = get(API, "/api/v1/miners")
    h.check("the miner list names the device of each connection", sorted({r["device"] for r in miners}) == ["Bitaxe", "MyOwnMiner", "NerdMiner"], str(sorted({r["device"] for r in miners})))
    # history: one sample per minute is stored; wait for the next minute change
    t0 = time.time(); pts = []
    while time.time() - t0 < 80:
        pts = get(PUB, "/public/v1/history?range=1h")["points"]
        if pts: break
        await asyncio.sleep(3)
    h.check("the history has a stored sample after a minute", len(pts) >= 1, str(pts[:1]))
    if pts:
        h.check("a sample is [time, hash rate, miners, ok, stale, dup, network difficulty]", len(pts[0]) == 7 and pts[0][2] >= 4, str(pts[0]))
    # history per device type (stored once per minute here)
    t0 = time.time(); dv = []
    while time.time() - t0 < 80:
        dv = get(PUB, "/public/v1/history/devices?range=1h")["devices"]
        if dv: break
        await asyncio.sleep(3)
    names = {d["name"]: d for d in dv}
    h.check("the history by device type has the device types", {"Bitaxe", "NerdMiner", "MyOwnMiner"} <= set(names), str(sorted(names)))
    if "Bitaxe" in names:
        pt = names["Bitaxe"]["points"][0]
        h.check("a device point is [time, hash rate, working connections] with the two Bitaxes", len(pt) == 3 and pt[2] == 2, str(pt))
    for r in ("24h", "7d", "30d", "all"):
        v = get(PUB, f"/public/v1/history/devices?range={r}")
        h.check(f"device history range {r} is served", v["range"] == r and isinstance(v["devices"], list))
    for r in ("24h", "7d", "30d", "all"):
        v = get(PUB, f"/public/v1/history?range={r}")
        h.check(f"range {r} is served", v["range"] == r and v["keepDays"] == 7 and isinstance(v["points"], list))
    for c in cs: c.w.close()

with Harness("step25") as h:
    cfg = h.node.datadir + "-cfg.toml"
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
