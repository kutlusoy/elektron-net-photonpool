"""The minimum payout amount can be changed while the pool runs (SIGHUP): the plans announce the values they used, the gateway follows
within its limits without reconfiguration, refuses plans beyond them, and a nonsensical value is not applied."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, re, signal, subprocess, time, urllib.request
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PHOTON = 18981, 18340

def addr(i): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]))
def summary(): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())

def pool_cfg(path, minimum):
    open(path, "w").write(f'''[node]
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
path = "{path}.db"
[api]
listen = "127.0.0.1:{API}"
ui_dir = "{os.path.join(ROOT, 'ui')}"
[photon]
enabled = true
listen = "127.0.0.1:{PHOTON}"
key_file = "{path}.key"
fee_address = "{addr(900)}"
fee_pct = 1.0
initial_difficulty = 0.0001
min_difficulty = 0.00001
min_output_leptons = {minimum}
''')

def wait(cond, secs=15):
    end = time.time() + secs
    while time.time() < end:
        try:
            if cond(): return True
        except Exception: pass
        time.sleep(0.2)
    return False

with Harness("step21", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    base = n.datadir + "-cfg"
    for suffix in (".toml.db", ".toml.db-wal", ".toml.db-shm", ".key"):
        if os.path.exists(base + suffix): os.remove(base + suffix)
    cfg = base + ".toml"
    pool_cfg(cfg, 100000)
    pool = subprocess.Popen([PHOTOND, "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool.log", "w"))
    gw = None
    try:
        h.check("the pool starts with the default minimum payout of 100,000 leptons", wait(lambda: summary()["photon"].get("minOutputLeptons") == 100000, 30), str(summary().get("photon")))
        key = summary()["photon"]["publicKey"]
        time.sleep(1.5)
        open(base + ".gw.toml", "w").write(f'''node_rpc_url = "http://127.0.0.1:{RPCPORT}"
node_auth = "t:t"
pool_addr = "127.0.0.1:{PHOTON}"
pool_key = "{key}"
network = "regtest"
payout_address = "{addr(5)}"
fee_address = "{addr(900)}"
fee_pct = 1.0
variants = 4
share_difficulty = 0.0001
software_miner = true
''')
        gw = subprocess.Popen([PHOTOND, "gateway", "--gateway-config", base + ".gw.toml"], stdout=subprocess.DEVNULL, stderr=open(base + ".gw.log", "w"))
        h.check("the gateway mines with the default minimum", wait(lambda: n.rpc("getblockcount") >= 135 and summary()["photon"]["announces"] >= 2, 30), str(summary()["photon"]))

        # raise within the gateway's limit (1,000,000 leptons by default): no reconfiguration of the gateway
        pool_cfg(cfg, 600000)
        pool.send_signal(signal.SIGHUP)
        h.check("SIGHUP applies the new minimum without a restart", wait(lambda: summary()["photon"]["minOutputLeptons"] == 600000, 10), str(summary()["photon"]))
        before = summary()["photon"]["announces"]
        height = n.rpc("getblockcount")
        for _ in range(6):
            time.sleep(1.5)
            if n.rpc("getblockcount") > height + 3: break
        h.check("the gateway follows the raised minimum and keeps mining", n.rpc("getblockcount") > height + 2 and summary()["photon"]["announces"] > before, f"height {n.rpc('getblockcount')} from {height}")
        log = open(base + ".gw.log").read()
        h.check("the gateway did not refuse any plan", "refusing plan" not in log, log[-300:])

        # beyond the gateway's limit: the plans are refused and the gateway stops until the pool is reasonable again
        pool_cfg(cfg, 5000000)
        pool.send_signal(signal.SIGHUP)
        h.check("a minimum above the gateway's limit is applied by the pool", wait(lambda: summary()["photon"]["minOutputLeptons"] == 5000000, 10))
        stalled_at = None
        for _ in range(12):
            time.sleep(1.5)
            if "refusing plan" in open(base + ".gw.log").read(): break
        log = open(base + ".gw.log").read()
        h.check("the gateway refuses plans with a minimum beyond its limit", "refusing plan" in log and "above the accepted" in log, log[-400:])
        stalled_at = n.rpc("getblockcount")
        time.sleep(6)
        h.check("without an acceptable plan the gateway stops finding blocks", n.rpc("getblockcount") <= stalled_at + 1, f"{stalled_at} -> {n.rpc('getblockcount')}")

        # a nonsensical value is refused as a whole and the old one stays
        pool_cfg(cfg, 900000000)
        pool.send_signal(signal.SIGHUP)
        time.sleep(1.5)
        h.check("a value above the sanity limit is not applied", summary()["photon"]["minOutputLeptons"] == 5000000, str(summary()["photon"]))
        h.check("the refusal is logged", "refused, keeping the old values" in open(base + ".pool.log").read())

        # back to a normal value: the gateway recovers by itself
        pool_cfg(cfg, 200000)
        pool.send_signal(signal.SIGHUP)
        h.check("the pool applies the lower minimum", wait(lambda: summary()["photon"]["minOutputLeptons"] == 200000, 10))
        # the plan for the next block was published with the old minimum and stays refused; the one after it follows the new value
        found = lambda: open(base + ".gw.log").read().count("submitted: None")
        before = found()
        for _ in range(2):
            n.rpc("generatetodescriptor", 1, "raw(51)")
            time.sleep(1.5)
        h.check("the gateway accepts the plans again and finds blocks itself", wait(lambda: found() > before, 25), f"gateway blocks {before} -> {found()}")
    finally:
        if gw and gw.poll() is None: gw.terminate()
        pool.terminate(); pool.wait(timeout=20)
