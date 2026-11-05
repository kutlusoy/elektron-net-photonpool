"""The node patch must never change consensus code: it may only ADD code (no line of existing code is removed or edited), only in the three files below, and
nothing in it may call into block or transaction validation for acceptance. Checked on the patch text; no node needed."""
import os, re, sys
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ALLOWED = {"src/rpc/mining.cpp", "src/validation.cpp", "src/validation.h"}
FORBIDDEN = [r"\bAcceptBlock\b", r"\bProcessNewBlock\b", r"\bConnectBlock\b", r"\bCheckBlock\b", r"\bContextualCheckBlock\b", r"\bnMuhashActivation", r"\bConsensus::Params&\s+\w+\s*=\s*const_cast",
             r"\bSetChainTip\b", r"\bFlushStateToDisk\b", r"\bInvalidateBlock\b", r"\bReconsiderBlock\b", r"\bPreciousBlock\b", r"\bAddCoin\(.*CoinsTip", r"\bm_chain\.SetTip\b"]
bad = 0
for name in sorted(os.listdir(os.path.join(ROOT, "docker", "node", "patches"))):
    if not name.endswith(".patch"): continue
    text = open(os.path.join(ROOT, "docker", "node", "patches", name)).read()
    files = set(re.findall(r"^diff --git a/(\S+) b/", text, re.M))
    removed = [l for l in text.splitlines() if l.startswith("-") and not l.startswith("---")]
    added = [l[1:] for l in text.splitlines() if l.startswith("+") and not l.startswith("+++")]
    hits = [p for p in FORBIDDEN for l in added if re.search(p, l)]
    ok = files <= ALLOWED and not removed and not hits
    print(f"  {'ok  ' if ok else 'FAIL'} {name}: files {sorted(files)}, removed lines {len(removed)}, forbidden calls {sorted(set(hits))}")
    bad += not ok
print(f"[check_patch_scope] {'PASSED' if not bad else 'FAILED'}")
sys.exit(1 if bad else 0)
