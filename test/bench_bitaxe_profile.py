import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, subprocess, threading, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.environ.get("TARGET_DIR", os.path.join(ROOT, "target", "release"))
API, STRATUM = 18981, 18333
COUNTS = [int(x) for x in os.environ.get("CONNS", "1000,5000,15000").split(",")]
AGENT = os.environ.get("AGENT", "bitaxe/BM1370/v2.9.0")
STEADY = os.environ.get("STEADY_SECS", "20")
# The synthetic miners send no valid shares, so the pool does not know their speed; pinning the interval gives them the cadence a Bitaxe with a hash rate estimate gets.
CADENCE = os.environ.get("CADENCE_MS", "200")
AUTOSCALE = os.environ.get("AUTOSCALE", "false")
ADDRS = int(os.environ.get("ADDRS", "64"))
SHARE_RATE = os.environ.get("SHARE_RATE", "0")

def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]

def write_cfg(path):
    addrs = [encode_segwit_address("bcrt", 0, list(prog(i))) for i in range(1, ADDRS + 1)]
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 20

[pool]
network = "regtest"
payout_addresses = {json.dumps(addrs)}
refresh_secs = 20
spot_check = 2
autoscale_candidates = {AUTOSCALE}
min_candidates = 16

[stratum]
listen = "127.0.0.1:{STRATUM}"
initial_difficulty = 0.001
max_connections = 100000
min_job_interval_ms = {CADENCE}
max_jobs_per_sec = 100000000
max_job_interval_ms = {CADENCE}

[ledger]
path = "{path}.db"

[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')

def rss_mb(pid):
    for l in open(f"/proc/{pid}/status"):
        if l.startswith("VmRSS"): return int(l.split()[1]) / 1024
    return 0

def cpu_s(pid):
    f = open(f"/proc/{pid}/stat").read().split(")")[1].split()
    return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")

account = encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"bench-account").digest()[:20]))
with Harness("bench-stratum") as h:
    cfg = h.node.datadir + "-cfg.toml"; write_cfg(cfg)
    d = subprocess.Popen([os.path.join(TARGET, "photond"), "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(300):
            try:
                s = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=2).read())
                if s["alloc"]["candidates"] == (16 if AUTOSCALE == 'true' else ADDRS): break
            except Exception: time.sleep(0.1)
        print(f"  info daemon ready with {ADDRS} candidates, rss {rss_mb(d.pid):.0f} MB")
        for n in COUNTS:
            lt = subprocess.Popen([os.path.join(TARGET, "loadtest"), "--addr", f"127.0.0.1:{STRATUM}", "--conns", str(n), "--account", account,
                                   "--share-rate", SHARE_RATE, "--ramp-per-sec", "4000", "--agent", AGENT, "--steady-secs", STEADY, "--wait-secs", "60"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            ready = json.loads(lt.stdout.readline())
            time.sleep(2)
            samples, stop = [], threading.Event()
            def poll():
                t_start = time.time()
                while not stop.wait(10):
                    try:
                        sm = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read()); a = sm["alloc"]; nd = sm["node"]
                        samples.append(f"{time.time() - t_start:.0f}s: {a['candidates']}/{nd.get('candidatesAvailable', '?')} candidates, {nd.get('demandUnitsPerSec', 0):.0f} units/s used, last build {nd['batchMs']} ms, overlap {a['overlap']}, epoch use {a['epochUse'] * 100:.0f}%")
                    except Exception: pass
            th = threading.Thread(target=poll, daemon=True)
            cpu0, t0c = cpu_s(d.pid), time.time()
            rss = rss_mb(d.pid)
            lt.stdin.write("go\n"); lt.stdin.flush()
            h.node.rpc("generatetodescriptor", 1, "raw(51)")
            th.start()
            res = json.loads(lt.stdout.readline())
            stop.set()
            cpu1, t1c = cpu_s(d.pid), time.time()
            summ = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())
            ms, st = res["ms"], res["steady"]
            print(f"  {n:>6} conns ({AGENT}): rss before {rss:.0f} MB, after {rss_mb(d.pid):.0f} MB | tip fan-out p50 {ms['p50']:.0f} max {ms['max']:.0f} ms ({res['received_clean']}/{res['connected']}) | "
                  f"steady {st['jobs_per_sec']:.0f} jobs/s = {st['mbit_per_sec']:.1f} Mbit/s to the miners, daemon cpu {(cpu1 - cpu0) / max(1e-9, t1c - t0c) * 100:.0f} % of one core | "
                  f"allocator at the end: {summ['alloc']['candidates']} candidates, overlap {summ['alloc']['overlap']}, epoch use {summ['alloc']['epochUse'] * 100:.0f} %")
            for line in samples: print("      " + line)
            lt.terminate(); lt.wait(timeout=20)
            time.sleep(2)
    finally:
        d.terminate(); d.wait(timeout=10)
