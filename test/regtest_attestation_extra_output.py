import base64, hashlib, json, struct, subprocess, sys, time, urllib.request, os, shutil

BIN = sys.argv[1]
DATADIR = sys.argv[2]
RPCPORT = 18999
AUTH = base64.b64encode(b"t:t").decode()

def rpc(method, *params):
    req = urllib.request.Request(f"http://127.0.0.1:{RPCPORT}/", json.dumps({"method": method, "params": list(params), "id": 1}).encode(),
                                 {"Authorization": "Basic " + AUTH, "Content-Type": "application/json"})
    try:
        r = json.load(urllib.request.urlopen(req))
    except urllib.error.HTTPError as e:
        r = json.load(e)
    if r.get("error"):
        raise RuntimeError(f"{method}: {r['error']}")
    return r["result"]

def sha256d(b): return hashlib.sha256(hashlib.sha256(b).digest()).digest()
def varint(n):
    if n < 0xfd: return bytes([n])
    if n <= 0xffff: return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)
def txout(value, script): return struct.pack("<q", value) + varint(len(script)) + script

def build_coinbase(t, payout_script, extra_outputs=(), script_sig_suffix=b""):
    height = t["height"]
    sig = bytes.fromhex(t["coinbase_script_sig_prefix"]) + script_sig_suffix
    outs = [txout(t["coinbasevalue"], payout_script)]
    for o in t["coinbase_required_outputs"]:
        outs.append(txout(o["value"], bytes.fromhex(o["scriptPubKey"])))
    for s in extra_outputs:
        outs.append(txout(0, s))
    vin = b"\x00" * 32 + b"\xff\xff\xff\xff" + varint(len(sig)) + sig + struct.pack("<I", 0xfffffffe)
    body = varint(1) + vin + varint(len(outs)) + b"".join(outs)
    lock = struct.pack("<I", height - 1)
    version = struct.pack("<i", 2)
    nowit = version + body + lock
    wit = version + b"\x00\x01" + body + varint(1) + varint(32) + b"\x00" * 32 + lock
    return nowit, wit

def solve_and_submit(t, nowit, wit):
    txs = t["transactions"]
    leaves = [sha256d(nowit)] + [bytes.fromhex(x["txid"])[::-1] for x in txs]
    while len(leaves) > 1:
        if len(leaves) % 2: leaves.append(leaves[-1])
        leaves = [sha256d(leaves[i] + leaves[i + 1]) for i in range(0, len(leaves), 2)]
    root = leaves[0]
    bits = bytes.fromhex(t["bits"])[::-1]
    target = int.from_bytes(bytes.fromhex(t["target"]), "big")
    head = struct.pack("<i", t["version"]) + bytes.fromhex(t["previousblockhash"])[::-1] + root + struct.pack("<I", t["curtime"]) + bits
    nonce = 0
    while True:
        h = sha256d(head + struct.pack("<I", nonce))
        if int.from_bytes(h[::-1], "big") <= target: break
        nonce += 1
    block = head + struct.pack("<I", nonce) + varint(1 + len(txs)) + wit + b"".join(bytes.fromhex(x["data"]) for x in txs)
    return rpc("submitblock", block.hex())

def gbt(): return rpc("getblocktemplate", {"rules": ["segwit"]})

shutil.rmtree(DATADIR, ignore_errors=True); os.makedirs(DATADIR)
node = subprocess.Popen([BIN, "-regtest", f"-datadir={DATADIR}", f"-rpcport={RPCPORT}", "-port=18998", "-rpcuser=t", "-rpcpassword=t",
                         "-listen=0", "-dnsseed=0", "-fixedseeds=0", "-persistmempool=0"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    for _ in range(120):
        try: rpc("getblockcount"); break
        except Exception: time.sleep(0.5)
    rpc("generatetodescriptor", int(sys.argv[3]) if len(sys.argv) > 3 else 130, "raw(51)")
    results = {}
    OPR = lambda payload: b"\x6a" + (bytes([len(payload)]) + payload if len(payload) < 0x4c else b"\x4c" + bytes([len(payload)]) + payload)
    cases = [
        ("control: unmodified coinbase", {}),
        ("append scriptSig byte (known invalid)", {"script_sig_suffix": b"\x00"}),
        ("append third OP_RETURN last (single push)", {"extra_outputs": [OPR(b"pool-tag-test")]}),
    ]
    for name, kw in cases:
        t = gbt()
        nowit, wit = build_coinbase(t, b"\x51", **kw)
        res = solve_and_submit(t, nowit, wit)
        print(f"height {t['height']:>4}  {name:<48} -> {res!r}")
        results[name] = res
    # accepted control must advance tip; others leave tip unchanged
    t = gbt()
    nowit, wit = build_coinbase(t, b"\x51")
    print(f"height {t['height']:>4}  {'control again':<48} -> {solve_and_submit(t, nowit, wit)!r}")
finally:
    try: rpc("stop")
    except Exception: node.terminate()
    node.wait(timeout=60)
