"""Traffic from the gateway to simulated ASICs with a coinbase of 200 payout outputs: one version-bit slice scheme (12 bits per unit, the pool style)
against the full 16 BIP 320 bits per unit. Every ASIC pretends to hash at ASIC_TH TH/s (fixed_hashrate, test knob); the clients only count what arrives.

Env: ASICS (default 150), ASIC_TH (default 200), SECONDS (measuring window, default 30)."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, signal, sqlite3, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PHOTON, GWSTRATUM = 18981, 18340, 18444
ASICS, ASIC_TH, WINDOW = int(os.environ.get("ASICS", "150")), float(os.environ.get("ASIC_TH", "200")), int(os.environ.get("SECONDS", "30"))

def addr(i): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]))
def summary(): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())
def up():
    try: return summary()["photon"].get("publicKey") and summary()["node"]["height"] > 0
    except Exception: return False

def pool_cfg(base):
    open(base + ".toml", "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 50
[pool]
network = "regtest"
payout_addresses = {json.dumps([addr(1), addr(2)])}
refresh_secs = 0
spot_check = 2
[stratum]
enabled = false
[ledger]
path = "{base}.toml.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
[photon]
enabled = true
listen = "127.0.0.1:{PHOTON}"
key_file = "{base}.key"
fee_address = "{addr(900)}"
fee_pct = 1.0
max_outputs = 200
''')

def gw_cfg(path, key, full_roll):
    open(path, "w").write(f'''node_rpc_url = "http://127.0.0.1:{RPCPORT}"
node_auth = "t:t"
pool_addr = "127.0.0.1:{PHOTON}"
pool_key = "{key}"
network = "regtest"
payout_address = "{addr(5)}"
fee_address = "{addr(900)}"
fee_pct = 1.0
max_outputs = 200
variants = 16
max_variants = 2048
[stratum]
listen = "127.0.0.1:{GWSTRATUM}"
initial_difficulty = 0.00001
full_version_roll = {"true" if full_roll else "false"}
fixed_hashrate = {ASIC_TH * 1e12}
''')

async def asic(i, stats, stop):
    r, w = await asyncio.open_connection("127.0.0.1", GWSTRATUM)
    for n, (m, p) in enumerate([("mining.configure", [["version-rolling"], {"version-rolling.mask": "ffffffff", "version-rolling.min-bit-count": 2}]), ("mining.subscribe", ["sim/1"]), ("mining.authorize", [f"sim.{i}", "x"])], 1):
        w.write((json.dumps({"id": n, "method": m, "params": p}) + "\n").encode())
    while not stop.is_set():
        try: line = await asyncio.wait_for(r.readline(), 5)
        except asyncio.TimeoutError: continue
        if not line: return
        stats["bytes"][i] += len(line)
        if b'"mining.notify"' in line:
            stats["jobs"][i] += 1; stats["last"] = len(line)
            if b'"mining.notify"' in line and line.rstrip().endswith(b"true]}"): stats["clean"][i] += 1
        elif b'version-rolling.mask' in line and "mask" not in stats:
            stats["mask"] = json.loads(line)["result"].get("version-rolling.mask")
    w.close()

async def measure(h, n, base, key, full_roll):
    gw_cfg(base + ".gw.toml", key, full_roll)
    gw = subprocess.Popen([PHOTOND, "gateway", "--gateway-config", base + ".gw.toml"], stdout=subprocess.DEVNULL, stderr=open(base + ".gw.log", "w"))
    try:
        await asyncio.sleep(4)
        stats = {"bytes": [0] * ASICS, "jobs": [0] * ASICS, "clean": [0] * ASICS, "last": 0}
        stop = asyncio.Event()
        tasks = [asyncio.create_task(asic(i, stats, stop)) for i in range(ASICS)]
        await asyncio.sleep(16)                      # connect, first jobs, the gateway grows the number of variants
        b0, j0 = sum(stats["bytes"]), sum(stats["jobs"])
        t0 = time.time()
        await asyncio.sleep(WINDOW)
        b1, j1 = sum(stats["bytes"]), sum(stats["jobs"])
        dt = time.time() - t0
        stop.set(); await asyncio.gather(*tasks, return_exceptions=True)
        log = open(base + ".gw.log").read()
        import re
        variants = [int(m) for m in re.findall(r"gateway: (\d+) variants for", re.sub(r"\x1b\[[0-9;]*m", "", log))]
        overlap = len(re.findall(r"used up", log))
        return {"mask": stats.get("mask"), "notify_bytes": stats["last"], "jobs_per_s_per_asic": (j1 - j0) / dt / ASICS, "kb_per_s_per_asic": (b1 - b0) / dt / ASICS / 1000,
                "total_mb_per_s": (b1 - b0) / dt / 1e6, "variants": variants[-1] if variants else 16, "overlap": overlap}
    finally:
        gw.terminate(); gw.wait(timeout=20)

with Harness("traffic", blocks=130, extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    base = n.datadir + "-cfg"
    for suffix in (".toml.db", ".toml.db-wal", ".toml.db-shm", ".key"):
        if os.path.exists(base + suffix): os.remove(base + suffix)
    pool_cfg(base)
    pool = subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool.log", "w"))
    try:
        for _ in range(200):
            if up(): break
            time.sleep(0.2)
        pool.send_signal(signal.SIGINT); pool.wait(timeout=30)
        # 250 Photon accounts with a credit, so that the plans have the full 200 outputs
        db = sqlite3.connect(base + ".toml.db")
        minute = int(time.time() // 60)
        db.executemany("INSERT OR REPLACE INTO share_buckets(minute, account, mode, diff, cnt) VALUES (?,?,?,?,?)", [(minute, p2wpkh_script(3000 + i).hex(), 3, 1.0 + (i % 17), 1) for i in range(250)])
        db.commit(); db.close()
        pool = subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool2.log", "w"))
        for _ in range(200):
            if up(): break
            time.sleep(0.2)
        key = summary()["photon"]["publicKey"]
        time.sleep(2)
        print(f"  info {ASICS} simulated ASICs at {ASIC_TH:.0f} TH/s each = {ASICS * ASIC_TH / 1000:.1f} PH/s, plans with up to 200 outputs, window {WINDOW} s")
        print("  mode                          | mask      | job size | variants | work space exhausted | jobs/s per ASIC | KB/s per ASIC | total MB/s")
        res = {}
        for name, full in (("slices (12 bits per unit)", False), ("full roll (16 bits per unit)", True)):
            r = asyncio.run(measure(h, n, base, key, full)); res[full] = r
            print(f"  {name:<29} | {r['mask']} | {r['notify_bytes']:>7} B | {r['variants']:>8} | {('yes' if r['overlap'] else 'no'):>19} | {r['jobs_per_s_per_asic']:>15.2f} | {r['kb_per_s_per_asic']:>13.1f} | {r['total_mb_per_s']:>9.2f}")
        ratio = res[False]["total_mb_per_s"] / max(res[True]["total_mb_per_s"], 1e-9)
        print(f"  info the full 16-bit roll sends {ratio:.1f} times less data")
        h.check("the full version roll reduces the traffic to the ASICs by at least a factor of 4", ratio >= 4, f"{ratio:.1f}")
        h.check("a job with a 200-output coinbase stays below the 16 KB line limit of ESP-Miner", res[True]["notify_bytes"] < 16384, str(res[True]["notify_bytes"]))
    finally:
        if pool.poll() is None: pool.terminate(); pool.wait(timeout=20)
