# Elektron Net: `elektron-net-photonpool` Payout Modes Guideline

- **Version:** 0.2 (draft; mode selection and solo direct payout implemented, ledger and payouts in progress)
- **Date:** November 3, 2026
- **Audience:** Pool developers and operators
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/node/miner.cpp` (coinbase construction), `src/consensus/consensus.h` (`COINBASE_MATURITY`) - treat as ground truth for anything the doc references
- **Consumer:** [`elektron-net-pool`](https://github.com/kutlusoy/elektron-net-pool) (solo) and [`elektron-net-ppool`](https://github.com/kutlusoy/elektron-net-ppool) (PPLNS), whose behaviour this guideline unifies in one daemon
- **See also:** [`guideline-stratum-v1.md`](./guideline-stratum-v1.md), [`guideline-daemon-core.md`](./guideline-daemon-core.md), [`mining-pool-integration.md`](https://github.com/kutlusoy/elektron-net/blob/main/doc-elektron/mining-pool-integration.md)

---

## 1. Goal

One pool process that offers solo, PPLNS and PPS side by side. A miner chooses its mode when it connects. Accounting, fees and risk limits are tracked per mode.

## 2. How a connection gets its mode

The mode is decided once, at `mining.authorize`, and stays fixed for the connection. Shares are tagged with the mode when they are accepted.

Order of precedence:

1. **Password option:** `m=solo`, `m=pplns` or `m=pps` in the Stratum password field, honoured only if the port allows overrides and the mode is enabled.
2. **Port default:** every listener has a default mode, for example `3333` = PPLNS, `3334` = PPS, `3335` = solo.
3. **Global default:** `payout.default_mode`.

Disabled modes are refused with a clear error (Stratum code 20, "mode not offered"). Miners that cannot set a password simply use the port that matches their mode.

```toml
[payout]
default_mode = "pplns"
modes = ["solo", "pplns", "pps"]
min_payout = 0.01              # in coins, per account (default)
max_solo_accounts = 1000

[payout.fee_pct]               # solo is paid directly in the coinbase and has no fee
pplns = 1.0                    # default
pps = 2.0                      # default

[payout.pplns]
window_factor = 2.0            # default: window = factor * network difficulty

[payout.pps]
enabled = true                 # offered, but paused until the reserve is funded
reserve_min = 5.0              # coins the pool wallet must hold above all liabilities
max_hashrate_share = 0.25      # cap of the pool hashrate that may mine PPS

[[stratum.ports]]
listen = "0.0.0.0:3333"
mode = "pplns"
allow_override = true
[[stratum.ports]]
listen = "0.0.0.0:3334"
mode = "pps"
[[stratum.ports]]
listen = "0.0.0.0:3335"
mode = "solo"
```

All values shown are the built-in defaults and stay configurable. PPS is refused with a clear message (`PPS is paused until the pool reserve is funded`) until the treasury guard allows it.

## 3. Where the money goes

**Solo: direct payout in the coinbase.** A solo connection gets its own candidate whose single payout output is the miner's own address. The attested coinbase is built for that address (one `getblocktemplate` call with `coinbaseaddress`, validated against the node with a `proposal` before it is used). The reward never touches the pool wallet. Because the coinbase has exactly one payout output, a solo block carries no pool fee. Candidates are built on demand when a solo account connects and pre-built after every tip change for recently active accounts. The number of distinct solo accounts per epoch is limited by `payout.max_solo_accounts` (default 1,000).

**PPLNS and PPS: pool wallet.** Every pool candidate pays a pool-owned address, so the reward lands in the pool wallet first and is paid out after coinbase maturity (`COINBASE_MATURITY` = 100 blocks). The finder's mode decides how it is distributed:

| Finder's mode | Block reward |
|---|---|
| solo | Paid directly to the finder in the coinbase, nothing to distribute |
| pplns | Split over the PPLNS window (last `window_factor * network difficulty` of PPLNS work), minus the PPLNS fee, at maturity |
| pps | Goes to the pool treasury (PPS miners are paid per share regardless of blocks) |

A miner's shares only count in the mode they were submitted under. Hashrate of a PPS miner never dilutes the PPLNS window and vice versa.

### PPS

Each accepted share of difficulty `d` is credited with `d / network_difficulty * block_subsidy * (1 - fee)`. Transaction fees are not part of the base (no fee variance); a later FPPS variant can add an average fee. The pool carries the variance: it needs a funded treasury and refuses new PPS connections when `reserve_min` is not met or `max_hashrate_share` is reached.

### PPLNS

At block maturity the reward minus fee is split over the shares inside the window, proportional to share difficulty. Credits stay pending until `min_payout`.

## 4. Ledger

Persistent store (SQLite first, no external service needed):

| Table | Content |
|---|---|
| `accounts` | payout address, creation time |
| `share_buckets` | per minute, account and mode: summed accepted difficulty (aggregated, to keep the write rate low) |
| `blocks` | height, hash, finder account and mode, reward, status (`found`, `maturing`, `mature`, `orphaned`) |
| `credits` | account, mode, amount, source (block or PPS share bucket), status |
| `payouts` | account, amount, transaction id, status (`pending`, `broadcast`, `confirmed`, `failed`) |

Share counters are flushed in batches from memory, never per share. The ledger is the only source of truth; the UI reads it through the admin API.

## 5. Payout worker

A separate task: selects accounts whose confirmed balance reaches `min_payout`, builds one batched transaction with the node wallet (RPC `sendmany`), records the payout before broadcasting and keeps it idempotent so a restart can never pay twice. Orphaned blocks reverse their pending credits. The node used for payouts needs a wallet; the wallet keys are not stored in the pool process.

## 6. Decisions taken

1. Solo miners are paid directly in the coinbase, no pool wallet involved, no fee.
2. Fees, PPLNS window and minimum payout use standard defaults (PPLNS 1 %, PPS 2 %, window factor 2.0, minimum 0.01) and remain configurable.
3. PPS is offered but starts paused until the pool wallet holds the configured reserve.
4. Payouts use a wallet on the pool's Elektron Net node (as in the existing stack), with a dry-run switch that is on by default.

## 7. Implementation plan

1. (done) Mode selection and solo direct payout in the Stratum server.
2. Ledger with aggregated share buckets and block tracking.
3. Maturity tracking and credit calculation per mode (unit tests with synthetic block and share histories).
4. Payout worker and its idempotency tests on regtest.
5. UI and API: per-mode statistics and per-account views.

## 8. Checklist

- [x] Operator decisions from section 6 made.
- [x] Mode selection and solo direct payout implemented and tested (`test/step10_modes_and_solo.py`).
- [ ] Ledger and credit calculation tested with synthetic histories.
- [ ] Payout worker tested against a regtest wallet, including restart safety.
- [ ] PPS reserve guard tested.
