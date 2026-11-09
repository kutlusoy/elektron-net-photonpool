import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, re
# A miner of an unknown firmware starts with header only mining (extranonce2_size 0). When its connections end without a share twice in a row (a firmware that refuses
# the handshake leaves within a second or two), the next connection of the same address and user agent gets extranonce space. A miner that delivers a share keeps its mode,
# and other user agents are not affected.
_src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "step33_pplns_scenarios.py")).read()
exec(_src[:_src.index("async def main")])

async def subscribe(agent, hold=0.0):
    c = await Client(POOLPORT).open()
    r = await c.request("mining.subscribe", [agent])
    if hold: await asyncio.sleep(hold)
    return c, r["result"]

async def main(h, pool, cfg):
    n = h.node
    n.rpc("createwallet", "photon")
    pool.start(); await ready()
    A = addr(b"fallback-account")
    agent = "MysteryMiner/3.1"
    c, res = await subscribe(agent)
    h.check("an unknown agent starts with header only mining", res[2] == 0 and res[1] == "", str(res))
    c.w.close(); await asyncio.sleep(0.5)
    c, res = await subscribe(agent)
    h.check("after one quick end it still does", res[2] == 0, str(res))
    c.w.close(); await asyncio.sleep(0.5)
    c, res = await subscribe(agent)
    h.check("after two quick ends it gets extranonce space (extranonce1 label, size 4)", res[2] == 4 and re.fullmatch(r"[0-9a-f]{8}", res[1]) is not None, str(res))
    h.check("the log says so", "the next ones get extranonce space" in pool.log_text())
    other, res2 = await subscribe("OtherFirmware/1.0")
    h.check("another user agent is not affected", res2[2] == 0, str(res2))
    other.w.close()
    # this one works: authorize, find a block with the extranonce rules; the mode is pinned
    r2 = await c.request("mining.authorize", [f"{A}.x", "x"])
    job = await c.wait_job()
    en1 = bytes.fromhex(res[1]); en2 = bytes.fromhex("0000002a")
    nonce, _ = grind_en(job, int(job[5], 16), en1 + en2)
    r3 = await c.request("mining.submit", [f"{A}.x", job[0], en2.hex(), job[7], f"{nonce:08x}"])
    h.check("a share in extranonce mode is accepted", r3["result"] is True, str(r3))
    c.w.close(); await asyncio.sleep(1)
    c, res = await subscribe(agent)
    h.check("a miner that delivered a share keeps its mode", res[2] == 4, str(res))
    c.w.close()
    # refused authorizations say nothing about the mode
    for i in range(3):
        x, r = await subscribe("BadUser/1.0"); await x.request("mining.authorize", ["not-an-address.w", "x"]); x.w.close(); await asyncio.sleep(0.4)
    x, r = await subscribe("BadUser/1.0")
    h.check("a miner whose authorization is refused stays in its mode", r[2] == 0, str(r))
    x.w.close()
    # a miner of the list that keeps leaving is switched to header only mining
    for i in range(3):
        c, r = await subscribe("cgminer/9.9"); c.w.close(); await asyncio.sleep(0.5)
    c, res = await subscribe("cgminer/9.9")
    h.check("an agent of the list keeps extranonce space after three quick ends", res[2] == 4, str(res))
    c.w.close(); await asyncio.sleep(0.5)
    c, res = await subscribe("cgminer/9.9")
    h.check("and is tried with header only mining after four", res[2] == 0, str(res))
    c.w.close()
    pool.stop()

def grind_en(job, version, ext):
    target = bits_target(int(job[6], 16))
    for nn in range(1 << 20):
        if int.from_bytes(sha256d(build_header(job, version, nn, extranonce=ext))[::-1], "big") <= target: return nn, None
    raise RuntimeError("no nonce")

with Harness("step40", extra=["-fallbackfee=0.0001"]) as h:
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
