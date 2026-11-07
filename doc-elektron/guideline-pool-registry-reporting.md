# Elektron Net - `elektron-net-photonpool` Pool Registry Reporting Guideline

- **Audience:** operators of Photon Pool, developers of explorers
- **Protocol:** [`elektron-net-registry`](https://github.com/kutlusoy/elektron-net-registry) `doc-elektron/integration-pools.md`
- **Code:** `src/registry.rs`, `identity_confirm` in `src/public.rs`, the hook after `BLOCK FOUND` in `src/stratum.rs`
- **Test:** `test/step34_registry_reporting.py`

## 1. What it does

Explorers (mempool instances) cannot tell from a block alone which pool found it, and the coinbase must stay untouched. The registry solves it off-chain:

1. The pool reads `mempools.txt` from the registry (every 15 minutes, with a copy on disk next to the ledger that is read first at start).
2. When the pool found a block (`submitblock` accepted) it sends `POST <explorer>/api/v1/pool-registry/report` with `{"name": "<pool name>", "blockHash": "<hash>"}` to every listed explorer. Fire and forget, 5 s timeout, no retry.
3. The explorer looks the name up in `pools.txt`, takes the pool's URL from there and calls `GET <pool url>/api/pool/identity/confirm?blockHash=<hash>`.
4. The pool answers `{"confirmed": true, "name": ..., "url": ...}` only for a block it found itself within the last 30 minutes, otherwise `confirmed: false`. Nobody else can make the pool say true, which is the whole trust model.

## 2. What the operator has to do

- **Name and address.** The pool name (`POOL_NAME` in `stack.conf`, `[public] name` or `[registry] name` in `photond.toml`) must be exactly the Name of the line in `pools.txt`, and the public page must be reachable under the URL of that line (`DOMAIN`). The line for Photon Pool is `"PHOTON", "Photon Pool", "https://phtn.space";`.
- **Registry entry.** Add the line by pull request to the registry repository. The registry lists its own criteria (live for a month, a minimum of active miners).
- **Switch.** `REGISTRY_REPORTING=true|false` in `stack.conf`, or `[registry] enabled` in `photond.toml`. Default on. Reporting only happens on mainnet, unless `[registry] url` is changed (forks, tests).
- **Reverse proxy.** The callback is plain HTTPS to the public page, so with the stack nothing is to do (Caddy forwards every path to the public listener). With your own proxy, forward `/api/pool/identity/confirm` as well.

## 3. What the pool logs

`registry: N explorers listed, this pool reports blocks as "<name>"` after each refresh, `registry: could not refresh ...` when the registry is unreachable (the last good list stays in use), and `registry: block <hash> not reported to <explorer>: <reason>` when one explorer did not answer. Nothing else in the pool depends on the answers.

## 4. Notes

- TLS uses the bundled Mozilla root certificates (`webpki-roots`), so a minimal container without `ca-certificates` works.
- A list that parses to zero entries (an error page, an empty file) never replaces a good list.
- Blocks found by Photon gateways (farms with their own node) are not reported by the pool, because the pool does not see them submitted.
