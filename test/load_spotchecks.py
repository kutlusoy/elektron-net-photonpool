"""Load of the attestation spot checks under many simulated Photon gateways (regtest, everything on one machine).

Env: GATEWAYS (comma list, default 100,500,2000), SECONDS (default 120), TIP_INTERVAL (default 10), TXS (mempool transactions per scenario, default 2500),
CHECKS_PER_TIP (pool spot check budget, default 8), CONC (concurrent checks, default 2), BLOCKMAXWEIGHT (default 300000, so a refilled mempool fills several consecutive blocks), VARIANTS (default 2), RATE (shares/s/gateway, default 0.2).
Prints one table row per scenario; not part of run_all.sh.
"""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, struct, subprocess, time, urllib.request
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PHOTON = 18981, 18340
SCEN = [int(x) for x in os.environ.get("GATEWAYS", "100,500,2000").split(",")]
SECONDS, INTERVAL = int(os.environ.get("SECONDS", "120")), float(os.environ.get("TIP_INTERVAL", "10"))
TXS, VARIANTS, RATE = int(os.environ.get("TXS", "2500")), int(os.environ.get("VARIANTS", "2")), float(os.environ.get("RATE", "0.2"))
BMW = os.environ.get("BLOCKMAXWEIGHT", "300000")
TICKS = os.sysconf("SC_CLK_TCK")

def addr(i): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]))
def summary(): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=10).read())

def cpu(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12])) / TICKS
    except Exception: return 0.0
def rss(pid):
    try:
        for l in open(f"/proc/{pid}/status"):
            if l.startswith("VmRSS"): return int(l.split()[1]) / 1024
    except Exception: pass
    return 0.0

def pool_cfg(path):
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
max_connections = 6000
max_checks_per_tip = {os.environ.get('CHECKS_PER_TIP', '8')}
max_concurrent_checks = {os.environ.get('CONC', '2')}
initial_difficulty = 0.0001
min_difficulty = 0.00001
''')

def fill(n, utxos, offset, count):
    sent = 0
    for txid, idx, value in utxos[offset:offset + count]:
        raw, _ = make_spend(txid, idx, value, outputs=2)
        n.rpc("sendrawtransaction", raw.hex()); sent += 1
    return sent

with Harness("loadspot", blocks=160 + (TXS * len(SCEN)) // 100 + 60, extra=["-acceptnonstdtxn=1", "-maxmempool=800", f"-blockmaxweight={BMW}", "-rpcworkqueue=512", "-rpcthreads=16"]) as h:
    n = h.node
    need = TXS * len(SCEN)
    fan = []
    for height in range(1, need // 100 + 2):
        cb = n.rpc("getblock", n.rpc("getblockhash", height), 2)["tx"][0]
        value = int(Decimal(str(cb["vout"][0]["value"])) * LEPTON)
        each = (value - 2000) // 100
        raw = struct.pack("<i", 2) + varint(1) + bytes.fromhex(cb["txid"])[::-1] + struct.pack("<I", 0) + b"\x00" + struct.pack("<I", 0xfffffffd) + varint(100) + b"".join(txout(each, b"\x51") for _ in range(100)) + struct.pack("<I", 0)
        n.rpc("sendrawtransaction", raw.hex()); fan.append((sha256d(raw), each))
    while n.rpc("getmempoolinfo")["size"] > 0:   # the block weight limit spreads the fan-outs over several blocks
        n.rpc("generatetodescriptor", 1, "raw(51)")
    utxos = [(t, i, e) for t, e in fan for i in range(100)]
    print(f"  info {len(utxos)} spendable outputs prepared, block weight limit {BMW}")
    print("  gateways | tips | checks/tip | check ms (total / node) | MB per check | pool CPU % | node CPU % | loadgen CPU % | pool RSS MB | shares acked | dropped")
    used = 0
    for N in SCEN:
        base = n.datadir + f"-cfg{N}"
        for suffix in (".toml.db", ".toml.db-wal", ".toml.db-shm"):
            if os.path.exists(base + suffix): os.remove(base + suffix)
        pool_cfg(base + ".toml")
        # reuse one pool key so the pin stays valid across scenarios
        sent = fill(n, utxos, used, TXS); used += TXS
        pool = subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool.log", "w"), env={**os.environ, "RUST_LOG": os.environ.get("POOL_LOG", "warn")})
        try:
            for _ in range(200):
                try:
                    if summary()["photon"].get("publicKey") and summary()["node"]["height"] > 0: break
                except Exception: pass
                time.sleep(0.2)
            key = summary()["photon"]["publicKey"]
            open(base + ".load.toml", "w").write(f'''pool_addr = "127.0.0.1:{PHOTON}"
pool_key = "{key}"
node_rpc_url = "http://127.0.0.1:{RPCPORT}"
node_auth = "t:t"
gateways = {N}
variants = {VARIANTS}
shares_per_sec = {RATE}
seconds = {SECONDS}
ramp_per_sec = 400
''')
            time.sleep(1.5)
            lg_log = open(base + ".load.log", "w")
            lg = subprocess.Popen([PHOTOND, "photon-load", "--load-config", base + ".load.toml"], stdout=subprocess.PIPE, stderr=lg_log, text=True)
            c0 = (cpu(pool.pid), cpu(n.proc.pid), cpu(lg.pid)); t0 = time.time(); tips = 0; rss_max = 0
            ramp = N / 400 + 5
            time.sleep(ramp)
            while time.time() - t0 < SECONDS - 3:
                time.sleep(INTERVAL)
                n.rpc("generatetodescriptor", 1, "raw(51)"); tips += 1
                rss_max = max(rss_max, rss(pool.pid))
            wall = time.time() - t0
            out, _ = lg.communicate(timeout=SECONDS + 120)
            c1 = (cpu(pool.pid), cpu(n.proc.pid), cpu(lg.pid) if lg.poll() is None else c0[2])
            s = summary()["photon"]
            rep = json.loads(out) if out.strip() else {}
            timed = max(1, s["checkTimed"])
            print(f"  {N:>8} | {tips:>4} | {s['checksOk'] + s['checksFailed'] + s['checksInconclusive']:>5} total ({(s['checksOk'] + s['checksFailed'] + s['checksInconclusive']) / max(1, tips):.1f}) | {s['checkTotalUs'] / timed / 1000:.0f} / {s['checkNodeUs'] / timed / 1000:.0f} | {s['checkBytes'] / timed / 1e6:.2f} | "
                  f"{(c1[0] - c0[0]) / wall * 100:.0f} | {(c1[1] - c0[1]) / wall * 100:.0f} | n/a | {rss_max:.0f} | {s['sharesOk']} | {rep.get('dropped')}")
            print(f"           ok {s['checksOk']} failed {s['checksFailed']} inconclusive {s['checksInconclusive']} skipped (budget) {s['checksSkipped']}, trust entries {s['trustEntries']}, sum of probabilities {s['sumP']:.1f}, connected {rep.get('connected')}/{N}, announces {rep.get('announces')}, shares sent {rep.get('shares_sent')} acked {rep.get('shares_acked')} rejected {rep.get('shares_rejected')}, "
                  f"template build {rep.get('template_build_ms_avg', 0):.0f} ms, {rep.get('tx_per_template_avg', 0):.0f} tx per template")
            h.check(f"{N} gateways: no spot check failed and nobody dropped", s["checksFailed"] == 0 and rep.get("dropped") == 0, str(s))
        finally:
            if pool.poll() is None: pool.terminate(); pool.wait(timeout=30)
