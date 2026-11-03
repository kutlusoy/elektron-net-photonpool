# Elektron Net: `elektron-net-photonpool` Payout Modes Guideline

- **Version:** 0.1 (design draft, not implemented)
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
min_payout = 0.01              # in coins, per account
fee_pct = { solo = 1.0, pplns = 1.0, pps = 2.0 }   # operator decision

[payout.pplns]
window_factor = 2.0            # window = factor * network difficulty, in difficulty-weighted shares

[payout.pps]
enabled = false                # off until the reserve is funded
reserve_min = 5.0              # coins the treasury must hold; PPS pauses below it
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

(The values above are examples. Fees and limits are operator decisions, no defaults are fixed yet.)

## 3. Where the money goes

The attestation fixes the coinbase to a single payout output, and every candidate pays a pool-owned address. Every block reward therefore lands in the pool wallet first and is paid out to miners after coinbase maturity (`COINBASE_MATURITY` = 100 blocks). Until then the pool holds the funds. The finder's mode decides how a block reward is distributed:

| Finder's mode | Block reward goes to |
|---|---|
| solo | The finder, minus the solo fee, at maturity |
| pplns | The PPLNS window: all PPLNS shares of the last `window_factor * network difficulty` of work, minus the PPLNS fee, at maturity |
| pps | The pool treasury (PPS miners are paid per share regardless of blocks) |

A miner's shares only count in the mode they were submitted under. Hashrate of a PPS miner never dilutes the PPLNS window and vice versa.

### PPS

Each accepted share of difficulty `d` is credited with `d / network_difficulty * block_subsidy * (1 - fee)`. Transaction fees are not part of the base (no fee variance); a later FPPS variant can add an average fee. The pool carries the variance: it needs a funded treasury and refuses new PPS connections when `reserve_min` is not met or `max_hashrate_share` is reached.

### PPLNS

At block maturity the reward minus fee is split over the shares inside the window, proportional to share difficulty. Credits stay pending until `min_payout`.

### Solo

The finder receives the reward minus fee. Other solo miners get nothing for that block; this is the expected solo behaviour.

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

## 6. Open decisions

1. Fees per mode and the default `min_payout`.
2. PPLNS `window_factor` (2.0 is a common choice, to be confirmed).
3. Whether PPS is offered at launch and the size of the reserve.
4. Whether solo miners receive the reward directly in the coinbase (one candidate per solo account, more templates) instead of through the pool wallet. The wallet route is simpler and is the proposal here.
5. Where the payout wallet lives (the mining node or a separate node).

## 7. Implementation plan

1. Mode selection and per-mode share tagging in the Stratum server.
2. Ledger with aggregated share buckets and block tracking.
3. Maturity tracking and credit calculation per mode (unit tests with synthetic block and share histories).
4. Payout worker and its idempotency tests on regtest.
5. UI and API: per-mode statistics and per-account views.

## 8. Checklist

- [ ] Operator decisions from section 6 made.
- [ ] Mode selection implemented and tested.
- [ ] Ledger and credit calculation tested with synthetic histories.
- [ ] Payout worker tested against a regtest wallet, including restart safety.
- [ ] PPS reserve guard tested.
