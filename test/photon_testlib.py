import base64, copy, hashlib, json, os, shutil, struct, subprocess, sys, time, urllib.error, urllib.request
from decimal import Decimal

SRC = os.environ.get("ELEKTRON_SRC", "/home/user/elektron-net")
sys.path.insert(0, os.path.join(SRC, "test", "functional", "test_framework"))
from crypto.muhash import data_to_num3072, MuHash3072  # noqa: E402

MOD = MuHash3072.MODULUS
RPCPORT = 18999
LEPTON = 10**8
WC_PREFIX = bytes.fromhex("6a24aa21a9ed")


def sha256d(b): return hashlib.sha256(hashlib.sha256(b).digest()).digest()
def varint(n):
    if n < 0xfd: return bytes([n])
    if n <= 0xffff: return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)
def push(data): return (bytes([len(data)]) if len(data) < 0x4c else b"\x4c" + bytes([len(data)])) + data
def op_return(payload): return b"\x6a" + push(payload)
def txout(value, script): return struct.pack("<q", value) + varint(len(script)) + script

def scriptnum_push(n):
    if 1 <= n <= 16: return bytes([0x50 + n])
    out = bytearray()
    while n:
        out.append(n & 0xff); n >>= 8
    if out[-1] & 0x80: out.append(0)
    return push(bytes(out))

def attestation_script(height, digest): return b"\x6a" + scriptnum_push(height) + push(digest)

def p2wpkh_script(i): return b"\x00\x14" + hashlib.sha256(b"photon-test-key-%d" % i).digest()[:20]


class Node:
    def __init__(self, binary, datadir, extra=(), rpcport=None, port=18998):
        self.binary, self.datadir, self.extra = binary, datadir, list(extra)
        self.rpcport, self.port = rpcport or RPCPORT, port
        self.auth = base64.b64encode(b"t:t").decode()
        self.proc = None

    def start(self):
        shutil.rmtree(self.datadir, ignore_errors=True); os.makedirs(self.datadir)
        self.proc = subprocess.Popen([self.binary, "-regtest", f"-datadir={self.datadir}", f"-rpcport={self.rpcport}", f"-port={self.port}",
                                      "-rpcuser=t", "-rpcpassword=t", "-listen=0", "-dnsseed=0", "-fixedseeds=0", "-persistmempool=0"] + self.extra,
                                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(240):
            try: self.rpc("getblockcount"); return
            except Exception: time.sleep(0.5)
        raise RuntimeError("node did not start")

    def stop(self):
        try: self.rpc("stop")
        except Exception: self.proc.terminate()
        self.proc.wait(timeout=60)

    def rpc(self, method, *params, wallet=None):
        body = json.dumps({"method": method, "params": list(params), "id": 1}).encode()
        req = urllib.request.Request(f"http://127.0.0.1:{self.rpcport}/" + (f"wallet/{wallet}" if wallet else ""), body, {"Authorization": "Basic " + self.auth, "Content-Type": "application/json"})
        try: raw = urllib.request.urlopen(req).read()
        except urllib.error.HTTPError as e: raw = e.read()
        r = json.loads(raw, parse_float=Decimal)
        if r.get("error"): raise RuntimeError(f"{method}: {r['error']}")
        return r["result"]

    def gbt(self): return self.rpc("getblocktemplate", {"rules": ["segwit"]})


class Harness:
    def __init__(self, name, blocks=130, extra=()):
        self.name, self.blocks, self.failed = name, blocks, 0
        self.node = Node(os.environ.get("ELEKTROND", sys.argv[1] if len(sys.argv) > 1 else "elektrond"),
                         os.environ.get("PHOTON_DATADIR", "/tmp/photon-test") + "-" + name, extra)

    def __enter__(self):
        self.node.start()
        self.node.rpc("generatetodescriptor", self.blocks, "raw(51)")
        return self

    def __exit__(self, exc_type, *a):
        self.node.stop()
        if exc_type is not None:
            import traceback
            traceback.print_exception(exc_type, *a)
        ok = not self.failed and exc_type is None
        print(f"[{self.name}] {'PASSED' if ok else 'FAILED'}")
        if not ok: sys.exit(1)

    def check(self, label, ok, detail=""):
        print(f"  {'ok  ' if ok else 'FAIL'} {label}" + (f"  ({detail})" if detail else ""))
        if not ok: self.failed += 1


def parse_tx(raw):
    p = 0
    def rd(n):
        nonlocal p; v = raw[p:p + n]; p += n; return v
    def rvi():
        nonlocal p
        b = raw[p]; p += 1
        if b < 0xfd: return b
        n = {0xfd: 2, 0xfe: 4, 0xff: 8}[b]
        return int.from_bytes(rd(n), "little")
    rd(4)
    assert raw[p] != 0, "segwit tx not supported by test parser"
    ins = []
    for _ in range(rvi()):
        prev = rd(32); n = struct.unpack("<I", rd(4))[0]; rd(rvi()); rd(4)
        ins.append((prev, n))
    outs = []
    for _ in range(rvi()):
        v = struct.unpack("<q", rd(8))[0]; outs.append((v, rd(rvi())))
    rd(4)
    return ins, outs


def is_unspendable(script): return script[:1] == b"\x6a" or len(script) > 10000

def coin_ser(txid_le, n, height, coinbase, value, script):
    return txid_le + struct.pack("<I", n) + struct.pack("<I", (height << 1) | int(coinbase)) + txout(value, script)

def coin_hash_num(ser): return data_to_num3072(hashlib.sha256(ser).digest())

def digest_of(num):
    return hashlib.sha256((num % MOD).to_bytes(384, "little")).digest()


def scan_utxos(node, scripts):
    descs = [f"raw({s.hex()})" for s in scripts]
    res = node.rpc("scantxoutset", "start", descs)
    coins = {}
    for u in res["unspents"]:
        key = (bytes.fromhex(u["txid"])[::-1], u["vout"])
        coins[key] = (int(Decimal(u["amount"]) * LEPTON), bytes.fromhex(u["scriptPubKey"]), u["height"], bool(u.get("coinbase", False)))
    return coins


class Oracle:
    def __init__(self, node, scripts):
        self.node = node
        self.coins = scan_utxos(node, scripts)

    def set_hash_num(self, coins):
        num = 1
        for (txid, n), (v, s, h, cb) in coins.items():
            num = num * coin_hash_num(coin_ser(txid, n, h, cb, v, s)) % MOD
        return num

    def base_after_txs(self, height, tx_raws):
        coins = dict(self.coins)
        for raw in tx_raws:
            ins, outs = parse_tx(raw)
            txid = sha256d(raw)
            for prev in ins: del coins[prev]
            for n, (v, s) in enumerate(outs):
                if not is_unspendable(s): coins[(txid, n)] = (v, s, height, False)
        return self.set_hash_num(coins)

    def candidate_digest(self, base_num, height, stripped_cb_txid, payout_value, payout_script, extra_spendable=()):
        num = base_num * coin_hash_num(coin_ser(stripped_cb_txid, 0, height, True, payout_value, payout_script)) % MOD
        for n, (v, s) in extra_spendable:
            num = num * coin_hash_num(coin_ser(stripped_cb_txid, n, height, True, v, s)) % MOD
        return digest_of(num)


def witness_commitment_script(tx_datas):
    leaves = [b"\x00" * 32] + [sha256d(d) for d in tx_datas]
    while len(leaves) > 1:
        if len(leaves) % 2: leaves.append(leaves[-1])
        leaves = [sha256d(leaves[i] + leaves[i + 1]) for i in range(0, len(leaves), 2)]
    return WC_PREFIX + sha256d(leaves[0] + b"\x00" * 32)


def build_coinbase(t, payout_script, extra_outputs=(), script_sig_suffix=b"", attestation=None, payout_value=None, tx_datas=None):
    sig = bytes.fromhex(t["coinbase_script_sig_prefix"]) + script_sig_suffix
    req = [bytes.fromhex(o["scriptPubKey"]) for o in t["coinbase_required_outputs"]]
    wc = next(s for s in req if s.startswith(WC_PREFIX))
    if tx_datas is not None: wc = witness_commitment_script(tx_datas)
    node_att = next(s for s in req if not s.startswith(WC_PREFIX))
    value = t["coinbasevalue"] if payout_value is None else payout_value
    vin = b"\x00" * 32 + b"\xff\xff\xff\xff" + varint(len(sig)) + sig + struct.pack("<I", 0xfffffffe)
    def ser(outs, wit):
        body = varint(1) + vin + varint(len(outs)) + b"".join(txout(v, s) for v, s in outs)
        lock = struct.pack("<I", t["height"] - 1)
        ver = struct.pack("<i", 2)
        if wit: return ver + b"\x00\x01" + body + varint(1) + varint(32) + b"\x00" * 32 + lock
        return ver + body + lock
    stripped = [(value, payout_script), (0, wc)] + [(0, s) for s in extra_outputs]
    att = attestation if attestation is not None else node_att
    final = [(value, payout_script), (0, att), (0, wc)] + [(0, s) for s in extra_outputs]
    return {"nowit": ser(final, False), "wit": ser(final, True), "stripped_txid": sha256d(ser(stripped, False)),
            "value": value, "node_attestation": node_att, "wc": wc}


def assemble_block(t, cb, tx_datas=None):
    txs = t["transactions"]
    datas = [bytes.fromhex(x["data"]) for x in txs] if tx_datas is None else tx_datas
    leaves = [sha256d(cb["nowit"])] + [sha256d(d) for d in datas]
    while len(leaves) > 1:
        if len(leaves) % 2: leaves.append(leaves[-1])
        leaves = [sha256d(leaves[i] + leaves[i + 1]) for i in range(0, len(leaves), 2)]
    head = (struct.pack("<i", t["version"]) + bytes.fromhex(t["previousblockhash"])[::-1] + leaves[0]
            + struct.pack("<I", t["curtime"]) + bytes.fromhex(t["bits"])[::-1])
    return head, cb["wit"], b"".join(datas), 1 + len(datas)


def solve(t, head, cbwit, rest, ntx):
    target = int.from_bytes(bytes.fromhex(t["target"]), "big")
    nonce = 0
    while int.from_bytes(sha256d(head + struct.pack("<I", nonce))[::-1], "big") > target: nonce += 1
    return (head + struct.pack("<I", nonce) + varint(ntx) + cbwit + rest).hex()


def block_hex_unsolved(head, cbwit, rest, ntx):
    return (head + b"\x00" * 4 + varint(ntx) + cbwit + rest).hex()


def proposal(node, hexblock):
    return node.rpc("getblocktemplate", {"mode": "proposal", "data": hexblock, "rules": ["segwit"]})


def submit(node, t, cb, tx_datas=None):
    return node.rpc("submitblock", solve(t, *assemble_block(t, cb, tx_datas)))


def make_spend(prev_txid_le, n, value, fee=1000, outputs=1):
    each = (value - fee) // outputs
    outs = b"".join(txout(each, b"\x51") for _ in range(outputs))
    raw = struct.pack("<i", 2) + varint(1) + prev_txid_le + struct.pack("<I", n) + b"\x00" + struct.pack("<I", 0xfffffffd) + varint(outputs) + outs + struct.pack("<I", 0)
    return raw, [(each, i) for i in range(outputs)]


def wrap_witness(cb_nowit, reserved):
    return cb_nowit[:4] + b"\x00\x01" + cb_nowit[4:-4] + varint(1) + varint(32) + reserved + cb_nowit[-4:]


def block_from_batch(bt, cand, datas):
    """Block parts (head, witness coinbase, rest, tx count) from a getattestationbatch response and one candidate."""
    cb = bytes.fromhex(cand["coinbase_hex"])
    wit = wrap_witness(cb, bytes.fromhex(bt.get("coinbase_witness", "00" * 32))[::-1])
    leaves = [sha256d(cb)] + [sha256d(d) for d in datas]
    while len(leaves) > 1:
        if len(leaves) % 2: leaves.append(leaves[-1])
        leaves = [sha256d(leaves[i] + leaves[i + 1]) for i in range(0, len(leaves), 2)]
    head = struct.pack("<i", bt["version"]) + bytes.fromhex(bt["tip"])[::-1] + leaves[0] + struct.pack("<I", bt["curtime"]) + bytes.fromhex(bt["bits"])[::-1]
    return head, wit, b"".join(datas), 1 + len(datas)


def solve_parts(bt, parts):
    head, wit, rest, ntx = parts
    target = int(bt["target"], 16); nonce = 0
    while int.from_bytes(sha256d(head + struct.pack("<I", nonce))[::-1], "big") > target: nonce += 1
    return (head + struct.pack("<I", nonce) + varint(ntx) + wit + rest).hex()
