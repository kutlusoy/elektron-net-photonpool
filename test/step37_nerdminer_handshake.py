import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, re
# Hobby firmware (NerdMiner) aborts on an empty extranonce1 ("Work aborted") and speaks plain Stratum V1: five submit parameters, no version rolling,
# a nonce without leading zeros, an empty extranonce2. The pool gives such agents a non-empty label as extranonce1 (extranonce2_size stays 0) and accepts the share.
_src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "step33_pplns_scenarios.py")).read()
exec(_src[:_src.index("async def main")])

async def main(h, pool, cfg):
    n = h.node
    n.rpc("createwallet", "photon")
    pool.start(); await ready()
    A = addr(b"nerd-account")
    for agent, label in (("NerdMinerV2/1.7.3", True), ("HAN_SOLOminer/1.0", True), ("bitaxe/BM1370/v2.9.0", False), ("cgminer/4.11", False)):
        c = await Client(POOLPORT).open()
        r = await c.request("mining.subscribe", [agent])
        res = r["result"]
        ok = (re.fullmatch(r"[0-9a-f]{8}", res[1]) is not None) if label else (res[1] == "")
        h.check(f"{agent}: extranonce1 {'is a label' if label else 'stays empty'}, extranonce2_size 0", ok and res[2] == 0, str(res))
        c.w.close()
    # a NerdMiner session end to end: no version rolling, five parameters, short nonce, empty extranonce2
    c = await Client(POOLPORT).open()
    r = await c.request("mining.subscribe", ["NerdMinerV2/1.7.3"])
    r2 = await c.request("mining.authorize", [f"{A}.nerd", "x"])
    h.check("the NerdMiner is authorized", r2["result"] is True, str(r2))
    job = await c.wait_job()
    h.check("it receives a job", job is not None)
    h0 = n.rpc("getblockcount")
    version = int(job[5], 16)
    nonce, _ = grind(job, version, want_block=True)
    r3 = await c.request("mining.submit", [f"{A}.nerd", job[0], "", job[7], f"{nonce:x}"])
    h.check("its share (five parameters, nonce without leading zeros) is accepted", r3["result"] is True, str(r3))
    ok = await wait_for(lambda: n.rpc("getblockcount") == h0 + 1, 15)
    h.check("and became a block of the node", ok is not None)
    await asyncio.sleep(1)
    h.check("the log has no complaint about this session", "authorization refused" not in pool.log_text())
    c.w.close()
    # a miner that connects, subscribes and leaves is named in the log
    d = await Client(POOLPORT).open()
    await d.request("mining.subscribe", ["SomeFirmware/0.1"])
    await asyncio.sleep(1.5)
    d.w.close()
    ok = await wait_for(lambda: "SomeFirmware/0.1" in pool.log_text() and "ended without a share" in pool.log_text(), 10)
    h.check("a session that ended without a share is logged with its user agent", ok is not None)
    e = await Client(POOLPORT).open()
    await e.request("mining.subscribe", ["x/1"])
    r4 = await e.request("mining.authorize", ["not-an-address.w", "x"])
    h.check("an invalid user name is refused", r4["result"] is False)
    ok = await wait_for(lambda: "authorization refused" in pool.log_text(), 10)
    h.check("and the log says why", ok is not None)
    e.w.close()
    pool.stop()

with Harness("step37", extra=["-fallbackfee=0.0001"]) as h:
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
