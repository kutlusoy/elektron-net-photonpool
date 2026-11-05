import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, subprocess, threading, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address

# Dynamic load: Bitaxe-style miners join and leave in waves while the pool sizes its work space (candidates) by itself.
# Schedule (seconds, connections to start, or a negative number of the oldest group to stop). Environment: ADDRS, SCALE (divides all counts), PHASE_SECS.
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.path.join(ROOT, "target", "release")
API, STRATUM = 18981, 18333
ADDRS = int(os.environ.get("ADDRS", "2000"))
SCALE = int(os.environ.get("SCALE", "1"))
PHASE = int(os.environ.get("PHASE_SECS", "40"))
AGENT = "bitaxe/BM1370/v2.9.0"
PLAN = [(0 * PHASE, 1000), (1 * PHASE, 4000), (2 * PHASE, 10000), (4 * PHASE, -1), (5 * PHASE, -1), (6 * PHASE, -1)]
END = 9 * PHASE

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
[stratum]
listen = "127.0.0.1:{STRATUM}"
initial_difficulty = 0.001
max_connections = 100000
min_job_interval_ms = 200
max_job_interval_ms = 200
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
with Harness("bench-dynamic") as h:
    cfg = h.node.datadir + "-cfg.toml"; write_cfg(cfg)
    d = subprocess.Popen([os.path.join(TARGET, "photond"), "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    groups, rows, stop = [], [], threading.Event()
    try:
        for _ in range(300):
            try:
                if json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=2).read())["alloc"]["candidates"] >= 1: break
            except Exception: time.sleep(0.1)
        t0 = time.time(); cpu_prev, t_prev = cpu_s(d.pid), t0
        plan = list(PLAN)
        while time.time() - t0 < END:
            t = time.time() - t0
            while plan and plan[0][0] <= t:
                _, n = plan.pop(0)
                if n > 0:
                    lt = subprocess.Popen([os.path.join(TARGET, "loadtest"), "--addr", f"127.0.0.1:{STRATUM}", "--conns", str(max(1, n // SCALE)), "--account", account, "--ramp-per-sec", "4000", "--agent", AGENT, "--steady-secs", str(END)],
                                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
                    lt.stdout.readline(); lt.stdin.write("go\n"); lt.stdin.flush(); groups.append((lt, n))
                elif groups:
                    lt, n = groups.pop(0); lt.terminate(); lt.wait(timeout=30)
            time.sleep(5)
            try:
                sm = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())
            except Exception: continue
            now = time.time(); cpu = cpu_s(d.pid)
            a, nd = sm["alloc"], sm["node"]
            rows.append(f"  t={now - t0:4.0f}s miners {sm['miners']['online']:>6} | candidates {a['candidates']:>5}/{nd['candidatesAvailable']} | demand {nd['demandUnitsPerSec']:>7.0f} units/s | overlap {a['overlap']:>6} | epoch use {a['epochUse'] * 100:3.0f}% | build {nd['batchMs']:>4} ms | rss {rss_mb(d.pid):4.0f} MB | cpu {(cpu - cpu_prev) / max(1e-9, now - t_prev) * 100:3.0f}%")
            print(rows[-1], flush=True); cpu_prev, t_prev = cpu, now
    finally:
        for lt, _ in groups:
            try: lt.terminate()
            except Exception: pass
        d.terminate(); d.wait(timeout=10)
