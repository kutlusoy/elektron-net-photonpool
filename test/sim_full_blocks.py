import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import json, struct, subprocess, time, urllib.request
from decimal import Decimal
from photon_testlib import *
from segwit_addr import encode_segwit_address

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGET = os.path.join(ROOT, "target", "release")
API, STRATUM = 18981, 18333
FANOUTS = int(os.environ.get("FANOUTS", "250"))      # fan-out transactions, 100 outputs each
ROUNDS = int(os.environ.get("ROUNDS", "2"))          # full blocks to simulate
TXS_PER_BLOCK = int(os.environ.get("TXS_PER_BLOCK", "11000"))
CONNS = int(os.environ.get("CONNS", "10000"))
ADDRS = int(os.environ.get("ADDRS", "430"))

def prog(i): return hashlib.sha256(b"sim-key-%d" % i).digest()[:20]
account = encode_segwit_address("bcrt", 0, list(hashlib.sha256(b"sim-account").digest()[:20]))

def fanout_tx(prev_txid, value, outputs=100, fee=2000):
    each = (value - fee) // outputs
    outs = b"".join(txout(each, b"\x51") for _ in range(outputs))
    raw = struct.pack("<i", 2) + varint(1) + prev_txid + struct.pack("<I", 0) + b"\x00" + struct.pack("<I", 0xfffffffd) + varint(outputs) + outs + struct.pack("<I", 0)
    return raw, each

def write_cfg(path):
    addrs = [encode_segwit_address("bcrt", 0, list(prog(i))) for i in range(ADDRS)]
    open(path, "w").write(f'''[node]
rpc_url = "http://127.0.0.1:{RPCPORT}"
auth = "t:t"
poll_ms = 20
[pool]
network = "regtest"
payout_addresses = {json.dumps(addrs)}
refresh_secs = 0
spot_check = 4
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

def api(path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{API}{path}", timeout=10).read())
def rss(pid):
    for l in open(f"/proc/{pid}/status"):
        if l.startswith("VmRSS"): return int(l.split()[1]) / 1024

with Harness("sim-full-blocks", blocks=FANOUTS + 330, extra=["-acceptnonstdtxn=1", "-maxmempool=800", "-limitancestorcount=100", "-limitdescendantcount=200"]) as h:
    n = h.node
    print(f"  info chain height {n.rpc('getblockcount')}, building {FANOUTS} fan-out transactions of 100 outputs")
    fan = []
    for height in range(1, FANOUTS + 1):
        cb = n.rpc("getblock", n.rpc("getblockhash", height), 2)["tx"][0]
        value = int(Decimal(str(cb["vout"][0]["value"])) * LEPTON)
        raw, each = fanout_tx(bytes.fromhex(cb["txid"])[::-1], value)
        n.rpc("sendrawtransaction", raw.hex())
        fan.append((sha256d(raw), each))
    n.rpc("generatetodescriptor", 1, "raw(51)")
    utxos = [(txid, i, each) for txid, each in fan for i in range(100)]
    print(f"  info UTXO set grew by {len(utxos)} spendable outputs")

    def fill(count, offset):
        t0 = time.time(); sent = 0
        for txid, idx, value in utxos[offset:offset + count]:
            raw, _ = make_spend(txid, idx, value, outputs=2)
            n.rpc("sendrawtransaction", raw.hex()); sent += 1
        return sent, time.time() - t0

    sent, dt = fill(TXS_PER_BLOCK, 0)
    info = n.rpc("getmempoolinfo")
    print(f"  info mempool filled: {info['size']} transactions, {info['bytes'] / 1e6:.2f} MB virtual size, {dt:.1f} s to submit")

    t0 = time.perf_counter(); gbt = n.gbt(); t_gbt = (time.perf_counter() - t0) * 1000
    weight = sum(len(bytes.fromhex(t["data"])) for t in gbt["transactions"])
    print(f"  info getblocktemplate with {len(gbt['transactions'])} transactions: {t_gbt:.0f} ms")
    for count in (1, 100, ADDRS, 2000):
        req = {"candidates": [{"outputs": [{"scriptPubKey": p2wpkh_script(i).hex()}]} for i in range(count)]}
        t0 = time.perf_counter(); r = n.rpc("getattestationbatch", req); dt = (time.perf_counter() - t0) * 1000
        print(f"  info getattestationbatch, {count:>4} candidates, {len(r['transactions'])} transactions in the block: {dt:.0f} ms")

    cfg = h.node.datadir + "-cfg.toml"; write_cfg(cfg)
    for backend in ("gbt", "batch"):
        s = open(cfg).read()
        s = s.replace('[pool]\n', f'[pool]\nbackend = "{backend}"\n', 1) if "backend" not in s else s
        open(cfg + backend, "w").write(s.replace('backend = "gbt"', f'backend = "{backend}"').replace('backend = "batch"', f'backend = "{backend}"'))
        r = subprocess.run([os.path.join(TARGET, "photond"), "-c", cfg + backend, "check"], capture_output=True, text=True, timeout=600)
        rep = json.loads(r.stdout) if r.returncode == 0 else {"fetch_ms": "failed " + r.stderr[-120:]}
        print(f"  info pool epoch build with {ADDRS} addresses and the full block, backend {backend}: {rep['fetch_ms']} ms (all validated)")

    d = subprocess.Popen([os.path.join(TARGET, "photond"), "-c", cfg, "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(600):
            try:
                if api("/api/v1/summary")["alloc"]["candidates"] == ADDRS: break
            except Exception: time.sleep(0.2)
        offset = TXS_PER_BLOCK
        for rnd in range(1, ROUNDS + 1):
            lt = subprocess.Popen([os.path.join(TARGET, "loadtest"), "--addr", f"127.0.0.1:{STRATUM}", "--conns", str(CONNS), "--account", account, "--ramp-per-sec", "4000"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            ready = json.loads(lt.stdout.readline())
            time.sleep(2)
            mem = rss(d.pid)
            lt.stdin.write("go\n"); lt.stdin.flush()
            t0 = time.perf_counter()
            n.rpc("generatetodescriptor", 1, "raw(51)")
            t_block = (time.perf_counter() - t0) * 1000
            res = json.loads(lt.stdout.readline())
            blk = n.rpc("getblock", n.rpc("getbestblockhash"), 1)
            s = api("/api/v1/summary")
            ms = res["ms"]
            print(f"  round {rnd}: block with {blk['nTx']} transactions, weight {blk['weight']}; {ready['connected']}/{CONNS} miners connected; node generated and connected the block in {t_block:.0f} ms;")
            print(f"           clean job at the miners (from the block request): p50 {ms['p50']:.0f} ms, p99 {ms['p99']:.0f} ms, max {ms['max']:.0f} ms, {res['received_clean']}/{res['connected']} received;")
            print(f"           pool tip-to-candidates-ready p50 {s['node']['p50']:.0f} ms, backend {s['node']['backend']}, daemon RSS {mem:.0f} MB -> {rss(d.pid):.0f} MB")
            lt.terminate(); lt.wait(timeout=30); time.sleep(2)
            if rnd < ROUNDS:
                sent, dt = fill(TXS_PER_BLOCK, offset); offset += TXS_PER_BLOCK
                print(f"  info refilled the mempool with {sent} transactions in {dt:.1f} s")
    finally:
        d.terminate(); d.wait(timeout=20)
