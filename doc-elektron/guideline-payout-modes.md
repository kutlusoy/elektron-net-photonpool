# Elektron Net: `elektron-net-photonpool` Payout Modes Guideline

- **Version:** 0.3 (draft; implemented and tested on regtest: mode selection, solo direct payout, ledger, PPLNS, PPS, maturity tracking, payouts)
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
min_payout_leptons = 100000    # per account (default, 0.001 coins)
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

SQLite (`ledger.path`, default `photond.db`) on a dedicated thread, no external service. Tables: `share_buckets` (per minute, account and mode: summed difficulty and count), `blocks` (finder, mode, reward, status `found`, `maturing`, `mature`, `orphaned`), `credits` (pending PPLNS credit per block and account), `balance` (confirmed and paid per account), `payouts` and `payout_items`.

Shares are aggregated in memory per session and flushed every few seconds in one transaction; nothing is written per share. Share history older than `ledger.keep_days` (default 14) is pruned.

## 5. Accounting rules (as implemented)

- **PPLNS:** when a PPLNS miner finds a block, the reward minus the PPLNS fee is split over the newest PPLNS shares up to `window_factor * network difficulty` of difficulty. The newest minute bucket that crosses the window boundary is taken proportionally. Credits stay `pending` until the block is mature (101 confirmations), become `confirmed` balance then, and are cancelled when the block is orphaned.
- **PPS:** every flushed PPS share row is credited immediately to the confirmed balance with `difficulty / network difficulty * subsidy * (1 - fee)`. A share is never credited more than a whole block (`subsidy * (1 - fee)`), which only matters on networks with a very low difficulty. The base is the block subsidy, transaction fees are not part of it.
- **Solo:** paid in the coinbase, only recorded in the ledger.
- **Treasury guard:** PPS is available only while `wallet balance - liabilities >= payout.pps.reserve_min` and the PPS share of the pool hashrate is within `max_hashrate_share`. The guard runs every payout interval; new PPS connections are refused with a clear message while it is off.

## 5.1 Payout worker

- Runs every `payout.interval_secs` (default 600), selects accounts with at least `payout.min_payout_leptons` and pays them in batches of at most `max_outputs_per_tx` with one `sendmany` per batch. The pool wallet pays the network fee. `payout.fee_rate` (sat/vB) fixes the rate when the node cannot estimate fees.
- **Dry run is the default.** With `payout.dry_run = true` the worker only logs what it would pay. Set it to `false` to pay for real.
- Safety: the balances are debited and a `pending` payout row is written before `sendmany`. A definitive RPC error reverts the balances, an unknown outcome (node unreachable) leaves the row pending. On every cycle pending rows are reconciled against the wallet through the unique comment `photon-payout-<id>`, so a crash can neither lose nor double-pay a batch. Repeated failures back off exponentially.
- The wallet can be encrypted: the passphrase comes from the environment variable named by `payout.wallet_passphrase_env`, the wallet is unlocked only while paying.
- With `payout.wallet` and `payout.wallet_addresses` the pool creates or loads the wallet and provisions its payout addresses itself (label `photon-pool`), so no address list has to be maintained.

## 6. Decisions taken

1. Solo miners are paid directly in the coinbase, no pool wallet involved, no fee.
2. Fees, PPLNS window and minimum payout use standard defaults (PPLNS 1 %, PPS 2 %, window factor 2.0, minimum payout 100,000 leptons) and remain configurable.
3. PPS is offered but starts paused until the pool wallet holds the configured reserve.
4. Payouts use a wallet on the pool's Elektron Net node (as in the existing stack), with a dry-run switch that is on by default.

## 7. Implementation status

1. Done: mode selection and solo direct payout (`test/step10_modes_and_solo.py`).
2. Done: ledger, PPLNS and PPS accounting, maturity tracking (unit tests in `src/ledger.rs`).
3. Done: payout worker with dry run, restart safety and wallet provisioning (`test/step11_ledger_and_payouts.py`, wallet-enabled node required).
4. Next: public pool page and API, per-mode statistics in the UI.

## 8. Checklist

- [x] Operator decisions from section 6 made.
- [x] Mode selection and solo direct payout implemented and tested (`test/step10_modes_and_solo.py`).
- [x] Ledger and credit calculation tested with synthetic histories.
- [x] Payout worker tested against a regtest wallet, including the dry-run to live switch and no double payout.
- [x] PPS reserve guard tested (refused while liabilities exceed the wallet balance, available when covered).
- [ ] Hardware and mainnet-scale tests.
