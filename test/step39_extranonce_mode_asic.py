import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, re, struct
# Firmware derived from cgminer (Antminer, Whatsminer, Avalon) refuses extranonce2_size 0 and always puts extranonce1 + extranonce2 into the coinbase. The pool gives such
# agents extranonce space: 8 bytes behind the attestation hash inside the attestation output, which the node ignores. This test is a client that behaves like cgminer:
# it accepts only extranonce2_size 2..16, builds the coinbase from coinb1 + extranonce1 + extranonce2 + coinb2, rolls extranonce2 and submits blocks, which the node has to accept.
_src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "step33_pplns_scenarios.py")).read()
exec(_src[:_src.index("async def main")])

async def cg_miner(account, agent="cgminer/4.12.0"):
    c = await Client(POOLPORT).open()
    r = await c.request("mining.subscribe", [agent])
    _, en1, n2 = r["result"]
    ok = bool(re.fullmatch(r"([0-9a-f]{2})*", en1)) and 2 <= n2 <= 16
    return c, en1, n2, ok

async def main(h, pool, cfg):
    n = h.node
    n.rpc("createwallet", "photon")
    pool.start(); await ready()
    A = addr(b"asic-account")
    c, en1, n2, ok = await cg_miner(A)
    h.check("a cgminer-like client accepts the subscribe answer (extranonce2_size between 2 and 16, extranonce1 hex)", ok, f"{en1!r} {n2}")
    r2 = await c.request("mining.authorize", [f"{A}.s19", "x"])
    h.check("it is authorized", r2["result"] is True)
    job = await c.wait_job()
    h.check("it receives a job", job is not None)
    # the pool's coinbase split: the extranonce lands where the coinbase script of the attestation ends
    c1, c2 = bytes.fromhex(job[2]), bytes.fromhex(job[3])
    h.check("coinb1 ends behind the 32 byte attestation hash, coinb2 holds the rest", len(c1) > 40 and len(c2) > 0)
    start = n.rpc("getblockcount")
    for i, en2 in enumerate((b"\x00\x00\x00\x01", b"\x00\x00\x00\x02", bytes.fromhex("deadbeef"))):
        job = await c.wait_job(after=job[0], timeout=8) or job
        version = int(job[5], 16)
        ext = bytes.fromhex(en1) + en2
        target = bits_target(int(job[6], 16))
        nonce = None
        for nn in range(1 << 20):
            val = int.from_bytes(sha256d(build_header(job, version, nn, extranonce=ext))[::-1], "big")
            if val <= target:
                nonce = nn; break
        assert nonce is not None
        r3 = await c.request("mining.submit", [f"{A}.s19", job[0], en2.hex(), job[7], f"{nonce:08x}"])
        h.check(f"block {i + 1} with extranonce2 {en2.hex()} is accepted by the pool", r3["result"] is True, str(r3))
        ok = await wait_for(lambda: n.rpc("getblockcount") == start + i + 1, 15)
        h.check(f"and by the node (height {start + i + 1})", ok is not None, str(n.rpc("getblockcount")))
        await asyncio.sleep(0.6)
    # a wrong extranonce2 length is refused, an unknown job too
    job = await c.wait_job(clean=True, after=job[0], timeout=8) or job
    r4 = await c.request("mining.submit", [f"{A}.s19", job[0], "00", job[7], "00000001"])
    h.check("an extranonce2 of the wrong length is refused", r4["result"] is None and r4["error"][0] == 20, str(r4))
    c.w.close()
    # the others keep header only mining
    c2_, en1b, n2b, okb = await cg_miner(A, "bitaxe/BM1370/v2.9.0")
    h.check("a Bitaxe still gets extranonce2_size 0", n2b == 0 and en1b == "", f"{en1b!r} {n2b}")
    c2_.w.close()
    pool.stop()

with Harness("step39", extra=["-fallbackfee=0.0001"]) as h:
    cfg = h.node.datadir + "-cfg.toml"; log = cfg + ".log"
    for ext in ("", "-wal", "-shm"):
        try: os.remove(cfg + ".db" + ext)
        except FileNotFoundError: pass
    try: os.remove(log)
    except FileNotFoundError: pass
    write_cfg(cfg)
    pool = Pool(cfg, "", log)
    try: asyncio.run(main(h, pool, cfg))
    finally: pool.stop()
