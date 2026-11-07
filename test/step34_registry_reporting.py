import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, threading, urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Block attribution through the registry: the pool reads mempools.txt, reports a block it found to every listed explorer, and answers the
# explorer's call back (/api/pool/identity/confirm) with true only for that block. The helpers of step33 (node harness, pool process, a miner that finds a block) are reused.
_src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "step33_pplns_scenarios.py")).read()
exec(_src[:_src.index("async def main")])

REG, PUB = 18990, 18982
reports, callbacks = [], []

class Fake(BaseHTTPRequestHandler):
    """Registry (mempools.txt) and explorer (report endpoint, which calls the pool back) in one."""
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path == "/mempools.txt":
            b = f'"Fake Mempool", "http://127.0.0.1:{REG}";\nnot a line\n'.encode()
            self.send_response(200); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
        else: self.send_response(404); self.end_headers()
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        reports.append(body)
        try:
            r = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{PUB}/api/pool/identity/confirm?blockHash={body['blockHash']}", timeout=5).read())
        except Exception as e: r = {"error": str(e)}
        callbacks.append(r)
        out = json.dumps({"attributed": bool(r.get("confirmed"))}).encode()
        self.send_response(200); self.send_header("Content-Length", str(len(out))); self.end_headers(); self.wfile.write(out)

def pub(path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{PUB}{path}", timeout=5).read())

async def main(h, pool, cfg):
    n = h.node
    n.rpc("createwallet", "photon")
    pool.start(); await ready()
    h.check("a hash nobody found is not confirmed", pub("/api/pool/identity/confirm?blockHash=" + "ab" * 32)["confirmed"] is False)
    h.check("a malformed hash is not confirmed", pub("/api/pool/identity/confirm?blockHash=zz")["confirmed"] is False)
    h.check("no hash at all is not confirmed", pub("/api/pool/identity/confirm")["confirmed"] is False)
    ok = await wait_for(lambda: "registry:" in pool.log_text() and "1 explorers listed" in pool.log_text(), 20)
    h.check("the pool reads the registry list (the bad line is skipped)", ok is not None)
    bhash, _ = await finder(h, addr(b"registry-miner"))
    ok = await wait_for(lambda: len(reports) == 1 and len(callbacks) == 1, 20)
    h.check("the explorer got exactly one report for the block", ok is not None, str(reports))
    h.check("the report names the pool as in pools.txt and carries the block hash", reports and reports[0] == {"name": "Registry Test Pool", "blockHash": bhash}, str(reports))
    h.check("the callback for that block is confirmed with name", callbacks and callbacks[0].get("confirmed") is True and callbacks[0].get("name") == "Registry Test Pool", str(callbacks))
    ok = await wait_for(lambda: "reported to Fake Mempool: attributed" in pool.log_text(), 10)
    h.check("the log says the explorer attributed the block", ok is not None)
    h.check("the same hash in upper case is confirmed too", pub("/api/pool/identity/confirm?blockHash=" + bhash.upper())["confirmed"] is True)
    h.check("another hash is still not confirmed", pub("/api/pool/identity/confirm?blockHash=" + "cd" * 32)["confirmed"] is False)
    h.check("the public API reports the release name", "release" in pub("/public/v1/pool"))
    pool.stop()
    # switched off: no report and nothing confirmed
    write_cfg(cfg, extra='[registry]\nenabled = false\n')
    reports.clear(); callbacks.clear()
    pool.start(); await ready()
    bhash2, _ = await finder(h, addr(b"registry-miner-2"))
    await asyncio.sleep(3)
    h.check("with the registry switched off nothing is reported", reports == [], str(reports))
    h.check("with the registry switched off nothing is confirmed", pub("/api/pool/identity/confirm?blockHash=" + bhash2)["confirmed"] is False)
    pool.stop()

srv = ThreadingHTTPServer(("127.0.0.1", REG), Fake)
threading.Thread(target=srv.serve_forever, daemon=True).start()
_orig = write_cfg
def write_cfg(path, min_payout=10000, max_outputs=100, extra=None):
    _orig(path, min_payout, max_outputs)
    open(path, "a").write(extra or f'[registry]\nenabled = true\nurl = "http://127.0.0.1:{REG}"\nname = "Registry Test Pool"\n')
    open(path, "a").write(f'\n[public]\nenabled = true\nlisten = "127.0.0.1:{PUB}"\nname = "Registry Test Pool"\n')

with Harness("step34", extra=["-fallbackfee=0.0001"]) as h:
    cfg = h.node.datadir + "-cfg.toml"; log = cfg + ".log"
    for ext in ("", "-wal", "-shm"):
        try: os.remove(cfg + ".db" + ext)
        except FileNotFoundError: pass
    try: os.remove(log)
    except FileNotFoundError: pass
    write_cfg(cfg)
    pool = Pool(cfg, "", log)
    try: asyncio.run(main(h, pool, cfg))
    finally: pool.stop(); srv.shutdown()
