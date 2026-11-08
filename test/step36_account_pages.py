import os, sys; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import asyncio, json, urllib.request
# The blocks and payouts of one address are available completely, page by page (the account view carries the first 20 and the totals).
_src = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "step33_pplns_scenarios.py")).read()
exec(_src[:_src.index("async def main")])

PUB = 18982
def pub(path): return json.loads(urllib.request.urlopen(f"http://127.0.0.1:{PUB}{path}", timeout=10).read())
def status(path):
    try: return urllib.request.urlopen(f"http://127.0.0.1:{PUB}{path}", timeout=10).status
    except urllib.error.HTTPError as e: return e.code

async def main(h, pool, cfg):
    n = h.node
    n.rpc("createwallet", "photon")
    pool.start(); await ready()
    A = addr(b"pages-A")
    N = 45
    for i in range(N): await finder(h, A)
    await asyncio.sleep(2)
    v = pub(f"/public/v1/account/{A}")
    h.check(f"the account view counts all {N} blocks", v["blocksTotal"] == N, str(v["blocksTotal"]))
    h.check("the account view carries the first 20", len(v["blocksFound"]) == 20)
    h.check("the totals by mode and by status add up", sum(v["blocksByMode"].values()) == N and sum(v["blocksByStatus"].values()) == N, f'{v["blocksByMode"]} {v["blocksByStatus"]}')
    p0 = pub(f"/public/v1/account/{A}/blocks?offset=0&limit=20")
    p1 = pub(f"/public/v1/account/{A}/blocks?offset=20&limit=20")
    p2 = pub(f"/public/v1/account/{A}/blocks?offset=40&limit=20")
    h.check("pages 1 to 3 hold 20, 20 and 5 blocks", (len(p0["items"]), len(p1["items"]), len(p2["items"])) == (20, 20, 5), str((len(p0["items"]), len(p1["items"]), len(p2["items"]))))
    heights = [b["height"] for p in (p0, p1, p2) for b in p["items"]]
    h.check("together they are all blocks, newest first, without a gap or a repeat", len(set(heights)) == N and heights == sorted(heights, reverse=True) and heights[0] - heights[-1] == N - 1, str(heights[:3]))
    h.check("the first page equals the blocks of the account view", [b["hash"] for b in p0["items"]] == [b["hash"] for b in v["blocksFound"]])
    h.check("a page after the end is empty and reports the total", pub(f"/public/v1/account/{A}/blocks?offset=100")["items"] == [] and pub(f"/public/v1/account/{A}/blocks?offset=100")["total"] == N)
    h.check("the credits are counted and paged like the blocks", v["creditsTotal"] == N and len(v["credits"]) == 20 and len(pub(f"/public/v1/account/{A}/credits?offset=40")["items"]) == 5 and pub(f"/public/v1/account/{A}/credits?offset=20")["total"] == N, str(v["creditsTotal"]))
    cr = [c["height"] for off in (0, 20, 40) for c in pub(f"/public/v1/account/{A}/credits?offset={off}")["items"]]
    h.check("all credits together: every block once, newest first, with the blocks to mature", len(set(cr)) == N and cr == sorted(cr, reverse=True) and pub(f"/public/v1/account/{A}/credits")["items"][0]["maturesIn"] > 0)
    pv = pub("/public/v1/pool")
    h.check("the pool reports the totals of blocks and payouts", pv["blocksTotal"] == N and pv["payoutsTotal"] == 0, f'{pv["blocksTotal"]} {pv["payoutsTotal"]}')
    pb = [len(pub(f"/public/v1/blocks?offset={o}&limit=20")) for o in (0, 20, 40, 60)]
    h.check("the pool's blocks come in pages (20, 20, 5, 0)", pb == [20, 20, 5, 0], str(pb))
    h.check("the pool's pages do not overlap", len({b["hash"] for o in (0, 20, 40) for b in pub(f"/public/v1/blocks?offset={o}&limit=20")}) == N)
    h.check("the page size is limited to 100", len(pub(f"/public/v1/account/{A}/blocks?limit=100000")["items"]) == N)
    h.check("an invalid address is a 400", status("/public/v1/account/nope/blocks") == 400 and status("/public/v1/account/nope/payouts") == 400)
    n.rpc("generatetodescriptor", 101, "raw(51)")
    ok = await wait_for(lambda: pub(f"/public/v1/account/{A}")["balance"]["paid"] > 0 and pub(f"/public/v1/account/{A}")["balance"]["confirmed"] == 0, 90)
    v = pub(f"/public/v1/account/{A}")
    h.check("payouts are counted and summed", v["payoutsTotal"] >= 1 and v["payoutsSum"] == v["balance"]["paid"], f'{v["payoutsTotal"]} {v["payoutsSum"]} {v["balance"]}')
    items = []
    off = 0
    while True:
        pg = pub(f"/public/v1/account/{A}/payouts?offset={off}&limit=2")
        if not pg["items"]: break
        items += pg["items"]; off += 2
    h.check("paging through the payouts with 2 per page finds all of them", len(items) == v["payoutsTotal"] and sum(x["amount"] for x in items) == v["payoutsSum"], f"{len(items)} of {v['payoutsTotal']}")
    pp = [x["time"] for o in range(0, v["payoutsTotal"], 1) for x in pub(f"/public/v1/payouts?offset={o}&limit=1")]
    h.check("the pool's payouts can be paged one by one", len(pp) == v["payoutsTotal"] == pub("/public/v1/pool")["payoutsTotal"], f"{len(pp)}")
    pool.stop()

_orig = write_cfg
def write_cfg(path, min_payout=10000, max_outputs=100):
    _orig(path, min_payout, max_outputs)
    open(path, "a").write(f'\n[public]\nenabled = true\nlisten = "127.0.0.1:{PUB}"\nname = "Pages Test"\n')

with Harness("step36", extra=["-fallbackfee=0.0001"]) as h:
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
