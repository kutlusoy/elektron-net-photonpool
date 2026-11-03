import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API = 18981

def prog(i): return hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]

def write_cfg(path, hrp="bcrt", network="regtest", count=8):
    addrs = [encode_segwit_address(hrp, 0, list(prog(i))) for i in range(1, count + 1)]
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 50

[pool]
network = "{network}"
payout_addresses = {json.dumps(addrs)}
refresh_secs = 0
spot_check = 4

[stratum]
enabled = false

[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
''')
    return addrs

def photond(cfg, *args, timeout=120):
    return subprocess.run([PHOTOND, "-c", cfg, *args], capture_output=True, text=True, timeout=timeout)

def get(path):
    return urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=5).read()

with Harness("step08") as h:
    n = h.node
    cfg = os.path.join(h.node.datadir + "-cfg.toml")
    write_cfg(cfg)
    scripts = {(b"\x00\x14" + prog(i)).hex() for i in range(1, 9)}

    r = photond(cfg, "check")
    rep = json.loads(r.stdout) if r.returncode == 0 else {}
    h.check("check: 8 candidates built and validated by the node", r.returncode == 0 and rep.get("candidates") == 8 and rep.get("rejected") == 0, (r.stderr or r.stdout)[-200:])
    print(f"  info candidate fetch for 8 addresses: {rep.get('fetch_ms')} ms via getblocktemplate")

    height0 = n.rpc("getblockcount")
    for i in range(3):
        r = photond(cfg, "check", "--mine")
        rep = json.loads(r.stdout) if r.returncode == 0 else {}
        ok = r.returncode == 0 and rep.get("mined", {}).get("accepted") is True
        h.check(f"check --mine #{i + 1}: block solved from a pool candidate is accepted by the node", ok, (r.stderr or r.stdout)[-200:])
    h.check("chain advanced by three blocks", n.rpc("getblockcount") == height0 + 3)
    for hh in range(height0 + 1, height0 + 4):
        blk = n.rpc("getblock", n.rpc("getblockhash", hh), 2)
        paid = blk["tx"][0]["vout"][0]["scriptPubKey"]["hex"]
        h.check(f"block {hh} coinbase pays one of the pool payout scripts", paid in scripts, paid[:14])

    bad_hrp = os.path.join(h.node.datadir + "-bad1.toml"); write_cfg(bad_hrp, hrp="be")
    r = photond(bad_hrp, "check")
    h.check("mainnet-prefixed addresses on a regtest config are refused", r.returncode != 0 and "expected bcrt" in r.stderr, r.stderr.strip()[-120:])
    bad_net = os.path.join(h.node.datadir + "-bad2.toml"); write_cfg(bad_net, hrp="be", network="mainnet")
    r = photond(bad_net, "check")
    h.check("config for another chain than the node is refused", r.returncode != 0 and "node is on chain regtest" in r.stderr, r.stderr.strip()[-120:])

    d = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        s = None
        for _ in range(100):
            try:
                s = json.loads(get("/api/v1/summary"))
                if s["alloc"]["candidates"] == 8: break
            except Exception: pass
            time.sleep(0.1)
        h.check("daemon publishes an epoch with 8 candidates", s is not None and s["alloc"]["candidates"] == 8)
        h.check("daemon height equals node height", s["node"]["height"] == n.rpc("getblockcount") + 1, f"{s['node']['height']}")
        h.check("node link reported up", s["node"]["connected"] is True)
        page = get("/").decode()
        h.check("daemon serves the UI", "PHOTON POOL" in page)
        t0 = time.time()
        n.rpc("generatetodescriptor", 1, "raw(51)")
        for _ in range(100):
            s2 = json.loads(get("/api/v1/summary"))
            if s2["node"]["height"] == s["node"]["height"] + 1: break
            time.sleep(0.02)
        dt = (time.time() - t0) * 1000
        h.check("daemon follows a new tip and rebuilds the epoch", s2["node"]["height"] == s["node"]["height"] + 1, f"{dt:.0f} ms observed from outside")
        h.check("tip to ready latency recorded", s2["node"]["samples"] >= 1, f"p50 {s2['node']['p50']:.0f} ms for {s2['alloc']['candidates']} candidates")
        print(f"  info tip>ready p50 {s2['node']['p50']:.0f} ms (8 candidates, getblocktemplate backend, 4 validated)")
        ev = urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/events", timeout=5)
        n.rpc("generatetodescriptor", 1, "raw(51)")
        seen = ""
        t1 = time.time()
        while time.time() - t1 < 5 and "epoch" not in seen: seen += ev.read1(4096).decode(errors="ignore") if hasattr(ev, "read1") else ev.read(64).decode(errors="ignore")
        h.check("event stream delivers epoch events", "epoch" in seen)
    finally:
        d.terminate(); d.wait(timeout=10)
