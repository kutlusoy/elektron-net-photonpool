import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, subprocess, time, urllib.error, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address
from stratum_testlib import *

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PUB, POOLPORT = 18981, 18982, 18333
def addr(label): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(label).digest()[:20]))
def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]
MINER = addr(b"public-test-miner")

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
spot_check = 2

[stratum]
listen = "127.0.0.1:{POOLPORT}"
initial_difficulty = 0.001

[ledger]
path = "{path}.db"

[public]
enabled = true
listen = "127.0.0.1:{PUB}"
name = "Test Pool"
stratum_host = "pool.example.org"
rate_per_min = 60

[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
explorer_url = "https://mempool.example/elektron"
''')

def http(port, path, headers=None):
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", headers=headers or {})
    try:
        r = urllib.request.urlopen(req, timeout=5)
        return r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()

async def main(h):
    for _ in range(200):
        try:
            s, _, b = http(PUB, "/public/v1/pool")
            if s == 200 and json.loads(b)["node"]["height"] > 0: break
        except Exception: pass
        await asyncio.sleep(0.1)
    s, hd, b = http(PUB, "/public/v1/pool")
    pool = json.loads(b)
    h.check("public pool endpoint answers", s == 200 and pool["name"] == "Test Pool" and pool["network"] == "regtest")
    h.check("connection details list the configured port and host", pool["stratum"]["host"] == "pool.example.org" and pool["stratum"]["ports"][0]["port"] == POOLPORT)
    h.check("modes show fees and PPS as paused without a wallet", {m["mode"]: (m["fee"], m["available"]) for m in pool["modes"]} == {"solo": (0.0, True), "pplns": (1.0, True), "pps": (2.0, False)}, str(pool["modes"]))
    h.check("security headers are set", "default-src 'none'" in hd.get("content-security-policy", "") and hd.get("x-content-type-options") == "nosniff" and hd.get("referrer-policy") == "no-referrer", str(hd))

    s, hd, b = http(PUB, "/")
    h.check("public listener serves the terminal page", s == 200 and b"PHOTON POOL" in b and "text/html" in hd.get("content-type", ""))
    for path in ("/style.css", "/common.js", "/public.js"):
        s, _, _ = http(PUB, path)
        h.check(f"static asset {path} is served", s == 200)
    for path in ("/index.html", "/app.js", "/api/v1/summary", "/api/v1/ledger", "/../Cargo.toml", "/public/v1/../../api/v1/ledger"):
        s, _, _ = http(PUB, path)
        h.check(f"operator resource {path} is NOT reachable on the public listener", s == 404, str(s))

    c, r = await setup_miner(POOLPORT, f"{MINER}.rigX")
    job = await c.wait_job()
    height0 = h.node.rpc("getblockcount")
    m, bhash = await mine_and_submit(c, f"{MINER}.rigX", job)
    for _ in range(100):
        if h.node.rpc("getblockcount") == height0 + 1: break
        await asyncio.sleep(0.05)
    await asyncio.sleep(1.2)
    s, _, b = http(PUB, "/public/v1/blocks")
    blocks = json.loads(b)
    h.check("a found block appears with a masked finder and an explorer link", blocks and blocks[0]["finder"] == MINER[:6] + ".." + MINER[-4:] and blocks[0]["url"].endswith(f"/block/{bhash}"), str(blocks[:1]))
    h.check("no full account address leaks through the blocks endpoint", MINER not in b.decode())
    s, _, b = http(PUB, "/public/v1/pool")
    leak = b.decode()
    h.check("the pool endpoint exposes no worker names", "rigX" not in leak and MINER not in leak)

    s, _, b = http(PUB, f"/public/v1/account/{MINER}")
    acct = json.loads(b)
    h.check("account lookup returns shares and balances for the address", s == 200 and acct["address"] == MINER and "pplns" in acct["shares24h"] and acct["workers"] >= 1, str(acct)[:200])
    s, _, _ = http(PUB, "/public/v1/account/not-an-address")
    h.check("an invalid address is rejected", s == 400)
    s, _, _ = http(PUB, "/public/v1/account/" + addr(b"someone-else")[:-3] + "abc")
    h.check("an address with a bad checksum is rejected", s == 400)

    spoof = [http(PUB, "/public/v1/pool", {"X-Forwarded-For": f"198.51.100.{i}"})[0] for i in range(100)]
    h.check("rate limit answers 429 with Retry-After and ignores X-Forwarded-For when no proxy is trusted", 429 in spoof and spoof.count(200) <= 60, f"{spoof.count(200)} ok, {spoof.count(429)} limited")
    s, hd, _ = http(PUB, "/public/v1/pool")
    h.check("the limited response carries Retry-After", s == 429 and hd.get("retry-after") == "10")
    s, _, _ = http(API, "/api/v1/summary")
    h.check("the operator API on its own listener is not affected by the public rate limit", s == 200)

with Harness("step12") as h:
    cfg = h.node.datadir + "-cfg.toml"
    for ext in ("", "-wal", "-shm"):
        try: os.remove(cfg + ".db" + ext)
        except FileNotFoundError: pass
    write_cfg(cfg)
    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(main(h))
    finally:
        d.terminate(); d.wait(timeout=10)
