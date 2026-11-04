"""Photon pool session end to end on regtest: pool (photond with [photon]), gateway (photond gateway) with its own patched node,
non-custodial coinbase payout, accounting proofs, attestation spot checks and a gateway that cheats."""
import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, subprocess, time, urllib.request
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PHOTOND = os.environ.get("PHOTOND", os.path.join(ROOT, "target", "release", "photond"))
API, PHOTON = 18981, 18340

def addr(i): return encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]))
def script(i): return p2wpkh_script(i)
def summary(): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}/api/v1/summary", timeout=5).read())

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
fee_pct = 1.0
min_ppm = 100
initial_difficulty = 0.0001
min_difficulty = 0.00001
''')

def gateway_cfg(path, key, miner, blocks=None, corrupt=False):
    open(path, "w").write(f'''node_rpc_url = "http://127.0.0.1:{RPCPORT}"
node_auth = "t:t"
pool_addr = "127.0.0.1:{PHOTON}"
pool_key = "{key}"
network = "regtest"
payout_address = "{addr(miner)}"
fee_address = "{addr(900)}"
fee_pct = 1.0
variants = 8
share_difficulty = 0.0001
software_miner = true
{f"stop_after_blocks = {blocks}" if blocks else ""}
corrupt_attestation = {"true" if corrupt else "false"}
''')

def coinbase_outputs(n, height):
    cb = n.rpc("getblock", n.rpc("getblockhash", height), 2)["tx"][0]
    return {o["scriptPubKey"]["hex"]: int(Decimal(str(o["value"])) * LEPTON) for o in cb["vout"] if o["value"] > 0}, [o["scriptPubKey"]["hex"] for o in cb["vout"]]

with Harness("step17", extra=["-acceptnonstdtxn=1"]) as h:
    n = h.node
    base = os.path.join(n.datadir + "-cfg")
    for suffix in (".toml.db", ".toml.db-wal", ".toml.db-shm", ".toml.key"):
        if os.path.exists(base + suffix): os.remove(base + suffix)
    pool_cfg(base + ".toml")
    pool = subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool.log", "w"))
    gws = []
    try:
        for _ in range(200):
            try:
                if summary()["photon"].get("publicKey") and summary()["node"]["height"] > 0: break
            except Exception: pass
            time.sleep(0.2)
        key = summary()["photon"]["publicKey"]
        h.check("the pool listens for Photon gateways and reports its key", len(key) == 64 and summary()["photon"]["enabled"])
        time.sleep(1.5)

        # honest gateway mines a few blocks; later plans pay it
        gateway_cfg(base + ".gw1.toml", key, 5, blocks=6)
        t0 = n.rpc("getblockcount")
        gw = subprocess.run([PHOTOND, "gateway", "--gateway-config", base + ".gw1.toml"], capture_output=True, text=True, timeout=240, env={**os.environ, "RUST_LOG": "info"})
        h.check("the honest gateway mined its blocks", n.rpc("getblockcount") >= t0 + 6 and gw.returncode == 0, gw.stderr[-400:])
        s = summary()["photon"]
        h.check("shares were accepted by the pool and none rejected as invalid coinbase", s["sharesOk"] > 0, str(s))
        h.check("no spot check of the fast-mining honest gateway failed (checks racing a new tip are inconclusive)", s["checksFailed"] == 0, str(s))
        h.check("the gateway verified its credit against the committed accounting root", "verified against the committed root" in gw.stderr, gw.stderr[-600:])

        # the last blocks pay the miner directly in the coinbase and carry the commitment
        paid_blocks, fee_ok, commit_ok = 0, True, True
        for height in range(t0 + 1, n.rpc("getblockcount") + 1):
            outs, scripts_ = coinbase_outputs(n, height)
            reward = sum(outs.values())
            if script(5).hex() in outs:
                paid_blocks += 1
                fee_ok &= outs[script(900).hex()] >= reward // 100
            commit_ok &= any(x.startswith("6a24" + b"PHOT".hex()) for x in scripts_)
        h.check(f"{paid_blocks} of the gateway's blocks pay the miner directly in the coinbase", paid_blocks >= 3)
        h.check("the pool fee output holds at least 1 % of the reward in those blocks", fee_ok)
        h.check("every block carries the accounting commitment", commit_ok)
        last, _ = coinbase_outputs(n, n.rpc("getblockcount"))
        h.check("the miner receives about the whole reward minus the fee in a late block", last.get(script(5).hex(), 0) >= int(sum(last.values()) * 0.9), str(last))

        # an honest gateway that does not find blocks: spot checks run on a stable tip and pass
        gateway_cfg(base + ".gw3.toml", key, 7)
        open(base + ".gw3.toml", "a").write("")
        s3 = open(base + ".gw3.toml").read().replace("software_miner = true", "software_miner = false")
        open(base + ".gw3.toml", "w").write(s3)
        honest = subprocess.Popen([PHOTOND, "gateway", "--gateway-config", base + ".gw3.toml"], stdout=subprocess.DEVNULL, stderr=open(base + ".gw3.log", "w"))
        gws.append(honest)
        time.sleep(3)
        ok0 = summary()["photon"]["checksOk"]
        for _ in range(16):
            n.rpc("generatetodescriptor", 1, "raw(51)")
            time.sleep(1.2)
            if summary()["photon"]["checksOk"] > ok0 + 1: break
        s = summary()["photon"]
        h.check("spot checks of an honest gateway pass on a stable tip", s["checksOk"] > ok0 and s["checksFailed"] == 0 and honest.poll() is None, str(s))
        honest.terminate()

        # a cheating gateway: corrupt attestation is caught by the spot check, shares are not credited, repeated cheating is banned
        gateway_cfg(base + ".gw2.toml", key, 6, corrupt=True)
        before = summary()["photon"]
        cheat = subprocess.Popen([PHOTOND, "gateway", "--gateway-config", base + ".gw2.toml"], stdout=subprocess.DEVNULL, stderr=open(base + ".gw2.log", "w"))
        gws.append(cheat)
        time.sleep(3)
        for _ in range(12):
            n.rpc("generatetodescriptor", 1, "raw(51)")
            time.sleep(1.2)
            if summary()["photon"]["checksFailed"] > before["checksFailed"] and cheat.poll() is not None: break
        after = summary()["photon"]
        h.check("the spot check catches the corrupted attestation", after["checksFailed"] > before["checksFailed"], str(after))
        for _ in range(40):
            if cheat.poll() is not None: break
            n.rpc("generatetodescriptor", 1, "raw(51)")
            time.sleep(1.2)
        h.check("a gateway that keeps cheating is disconnected", cheat.poll() is not None)
        s = summary()["photon"]
        h.check("the pool remembers trust per address and per payout script", s["trustEntries"] >= 2, str(s))

        # the ban survives a reconnect ...
        again = subprocess.run([PHOTOND, "gateway", "--gateway-config", base + ".gw2.toml"], capture_output=True, text=True, timeout=60)
        h.check("the banned gateway is refused when it reconnects", again.returncode != 0 and "banned" in again.stderr, again.stderr[-300:])

        # ... and a restart of the pool (trust is written to the ledger on shutdown)
        import signal
        pool.send_signal(signal.SIGINT); pool.wait(timeout=30)
        pool = subprocess.Popen([PHOTOND, "-c", base + ".toml", "run"], stdout=subprocess.DEVNULL, stderr=open(base + ".pool2.log", "w"))
        for _ in range(200):
            try:
                if summary()["photon"].get("publicKey") and summary()["node"]["height"] > 0: break
            except Exception: pass
            time.sleep(0.2)
        time.sleep(1.5)
        h.check("remembered trust is loaded after a restart", summary()["photon"]["trustEntries"] >= 2, str(summary()["photon"]))
        again = subprocess.run([PHOTOND, "gateway", "--gateway-config", base + ".gw2.toml"], capture_output=True, text=True, timeout=60)
        h.check("the banned gateway is still refused after the restart", again.returncode != 0 and "banned" in again.stderr, again.stderr[-300:])
        ok_gw = subprocess.Popen([PHOTOND, "gateway", "--gateway-config", base + ".gw3.toml"], stdout=subprocess.DEVNULL, stderr=open(base + ".gw3b.log", "w"))
        gws.append(ok_gw)
        time.sleep(4)
        h.check("the honest gateway connects again and its trust is reused", ok_gw.poll() is None and summary()["photon"]["channels"] >= 1, str(summary()["photon"]))
    finally:
        for g in gws:
            if g.poll() is None: g.terminate()
        pool.terminate(); pool.wait(timeout=20)
