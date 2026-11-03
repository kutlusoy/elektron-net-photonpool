import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.path.join(ROOT, "target", "release")
API, STRATUM = 18981, 18333
COUNTS = [int(x) for x in os.environ.get("CONNS", "1000,5000,15000").split(",")]
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
refresh_secs = 0
spot_check = 2

[stratum]
listen = "127.0.0.1:{STRATUM}"
initial_difficulty = 0.001
max_connections = 100000

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
                if s["alloc"]["candidates"] == ADDRS: break
            except Exception: time.sleep(0.1)
        print(f"  info daemon ready with {ADDRS} candidates, rss {rss_mb(d.pid):.0f} MB")
        for n in COUNTS:
            lt = subprocess.Popen([os.path.join(TARGET, "loadtest"), "--addr", f"127.0.0.1:{STRATUM}", "--conns", str(n), "--account", account,
                                   "--share-rate", SHARE_RATE, "--ramp-per-sec", "4000"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            ready = json.loads(lt.stdout.readline())
            time.sleep(2)
            cpu0, t0c = cpu_s(d.pid), time.time()
            rss = rss_mb(d.pid)
            lt.stdin.write("go\n"); lt.stdin.flush()
            t_go = time.time()
            n_.rpc("generatetodescriptor", 1, "raw(51)") if False else h.node.rpc("generatetodescriptor", 1, "raw(51)")
            res = json.loads(lt.stdout.readline())
            cpu1 = cpu_s(d.pid)
            summ = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())
            ms = res["ms"]
            print(f"  {n:>6} conns: connected {ready['connected']} failed {ready['failed']} in {ready['connect_secs']:.1f}s | daemon rss {rss:.0f} MB | "
                  f"clean job after new tip (from loadtest 'go'): p50 {ms['p50']:.0f} p90 {ms['p90']:.0f} p99 {ms['p99']:.0f} max {ms['max']:.0f} ms, "
                  f"{res['received_clean']}/{res['connected']} received | daemon cpu during fan-out {cpu1 - cpu0:.2f}s | api sessions {summ['miners']['total']}")
            lt.terminate(); lt.wait(timeout=20)
            time.sleep(2)
    finally:
        d.terminate(); d.wait(timeout=10)
