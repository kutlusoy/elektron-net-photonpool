import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, re
# What open source mining firmware does with the pool's subscribe answer. For most agents (Bitaxe, NerdMiner) extranonce2_size is 0; agents of the cgminer family get
# extranonce space (4 + 4 bytes, see step39). The rules below are copied from the source code of
# the clients (initiate_stratum / parse_subscribe_result); the firmware itself is not run. The expectation is the status of the compatibility table in
# doc-elektron/guideline-stratum-v1.md: when the pool or a client changes, this matrix has to change with it.
#  cgminer, Bitmain bmminer (Antminer): util.c  "if (n2size < 2 || n2size > 16) Failed to get valid n2size in initiate_stratum"
#  sgminer, bfgminer:                   util.c  "if (n2size < 1) Failed to get n2size in initiate_stratum"
#  cpuminer (pooler):                   util.c  "if (!xn2_size) Failed to get extranonce2_size"; 0 < size <= 100
#  ESP-Miner (Bitaxe):                  stratum_api.c  extranonce_2_len clamped to 0..N, accepts 0
#  NerdMiner (Elektron fork):           stratum.cpp  aborts on an empty extranonce1 only; utils.cpp skips both extranonces when the size is 0
_src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "step33_pplns_scenarios.py")).read()
exec(_src[:_src.index("async def main")])

hexok = lambda s: isinstance(s, str) and re.fullmatch(r"([0-9a-fA-F]{2})*", s) is not None
FIRMWARE = [
    ("cgminer", "cgminer/4.12.0", lambda r: hexok(r[1]) and 2 <= r[2] <= 16, True),
    ("bmminer (Antminer stock)", "bmminer/2.0.0", lambda r: hexok(r[1]) and 2 <= r[2] <= 16, True),
    ("sgminer / bfgminer", "bfgminer/5.5.0", lambda r: r[2] >= 1, True),
    ("cpuminer (pooler)", "cpuminer/2.5.1", lambda r: isinstance(r[1], str) and 0 < r[2] <= 100, True),
    ("Whatsminer (user agent from the pool log)", "whatsminer/v1.1", lambda r: hexok(r[1]) and 2 <= r[2] <= 16, True),
    ("ESP-Miner (Bitaxe)", "bitaxe/BM1370/v2.9.0", lambda r: 0 <= r[2] <= 32, True),
    ("NerdMiner (Elektron fork)", "NerdMinerV2/1.7.3", lambda r: len(r[1]) > 0, True),
]

async def main(h, pool, cfg):
    n = h.node
    n.rpc("createwallet", "photon")
    pool.start(); await ready()
    for name, agent, accepts, expected in FIRMWARE:
        c = await Client(POOLPORT).open()
        r = await c.request("mining.subscribe", [agent])
        got = accepts(r["result"])
        h.check(f"{name}: {'accepts' if expected else 'rejects'} the pool's subscribe answer ({r['result'][1]!r}, size {r['result'][2]})", got == expected, str(r["result"]))
        c.w.close()
    pool.stop()

with Harness("step38", extra=["-fallbackfee=0.0001"]) as h:
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
