# Elektron Net: `elektron-net-photonpool` Admin API and Terminal UI Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool developers and operators
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/interfaces/mining.h` (tip, template and block data the pool exposes) - treat as ground truth for anything the doc references
- **See also:** [`guideline-photon-architecture.md`](./guideline-photon-architecture.md), [`guideline-batch-attestation-rpc.md`](./guideline-batch-attestation-rpc.md)

---

## 1. Purpose

The web UI (`ui/`) is one terminal-style single-page app: static HTML, CSS and JavaScript without dependencies and without a build step. The same files are served on the public listener and on the operator listener. Views: **POOL**, **ENGINE** and **MINER** read only the public data API (`/public/v1/`) and exist on both ports; the **OPS** view (workers with full names, blocks, ledger, console) exists only where the read-only admin API below answers, i.e. on the operator listener. The pool daemon serves the files; it MUST run fully without the UI.

## 2. UI behaviour

- OPS view panes: miners, blocks, ledger and a console (TTY) with a command line. The chain tip and allocator numbers, the share quality and a sanitized event feed (epoch, tip, candidates ready in N ms, blocks found with a masked address) are in the ENGINE view, which is public. With `api.explorer_url` set, the chain tip hash and the block heights in the blocks pane (and the output of `tip` and `blocks`) are links to the explorer.
- Commands: `help`, `status`, `tip`, `miners [n] [sort k] [grep s] [reset]`, `blocks`, `alloc`, `log [n]`, `watch`, `theme cyan|amber|green|magenta`, `clear`, `about`. Tab completes, Up and Down recall history, Ctrl+L clears.
- On load the shell probes `/api/v1/summary`. Only if it answers, `ops.js` is loaded and the OPS entry appears in the title bar; on the public listener the probe gets 404 and `ops.js` is not served at all, so the operator view cannot be reached there. Opening `/ops` on the public port falls back to the pool view.
- The layout follows the viewport (three columns on wide screens, stacked on phones). Animations stop with `prefers-reduced-motion`. Theme choice is kept in `localStorage` when available.
- The UI never sends commands that change pool state. All commands are local views.

## 3. Admin API (read-only, v1)

All responses are JSON. Authentication and binding are described in section 4.

| Endpoint | Content |
|---|---|
| `GET /api/v1/summary` | `pool {name, version, uptime, mode, explorerUrl}`, `node {height, tip, tipTime, connected, transport, rttMs, p50, p95, samples, batchMs}`, `hashrate {now, m1, h1, hist[]}`, `miners {online, total, v1, v2, edge}`, `shares {perSec, ok, stale, dup, inv, total}`, `alloc {candidates, unitsPerSec, epochUse, overlap}`, `found` |
| `GET /api/v1/miners?limit=n` | array of `{name, proto, hashrate, diff, ok, stale, dup, seen, unit, agent, device, best, jobMs}` (`agent` is the user agent sent with `mining.subscribe`, `device` its display name, `best` the best share difficulty of the connection, `jobMs` the job interval the pool chose for it) |
| `GET /api/v1/devices` | the same device list as the public API (`devices[]` with `name`, `url`, `openSource`, `working`, `hashrate`, `best`) |
| `GET /api/v1/history/devices?range=1h\|24h\|7d\|30d\|all` | stored hash rate and working connections per device type, the same as the public API |
| `GET /api/v1/history?range=1h\|24h\|7d\|30d\|all` | stored pool statistics, the same as the public API |
| `GET /api/v1/blocks?limit=n` | array of `{height, hash, time, worker, status, url?}` with status `found`, `maturing` or `mature`; `url` is `<explorer_url>/block/<hash>` and only present when `api.explorer_url` is configured |
| `GET /api/v1/events` | Server-Sent Events, each event a JSON object `{t, lvl, msg}` with `lvl` one of `info`, `share`, `job`, `warn`, `err`, `blk` |

Units: hashrate in hashes per second, times as Unix seconds (`tipTime`, `time`, `t`) or seconds (`uptime`, `seen`), ratios between 0 and 1. `tip>job` latencies (`p50`, `p95`) are milliseconds from tip change to the first job on the wire.

## 4. Security

1. The API and the static UI MUST bind to a loopback or management interface by default, never to the miner-facing ports.
2. Access MUST require authentication (token or mutual TLS) once bound beyond loopback.
3. Responses MUST be read-only and MUST NOT contain payout keys, RPC credentials, worker passwords or the node cookie.
4. The server SHOULD send a strict `Content-Security-Policy` (`default-src 'self'`) and `X-Content-Type-Options: nosniff`.
5. Worker names and other miner-supplied text are untrusted. The UI escapes all dynamic text before rendering.
6. The event stream and `limit` parameters MUST be bounded server-side.
8. The public listener MUST NOT serve the admin API or `ops.js`. What it shows of the engine is aggregated (numbers) or whitelisted text (the event feed, `Pool::public_message`: no worker names, no unmasked addresses, no node errors); `step26` proves that no worker name, address or node detail appears in `/public/v1/`.
7. `api.explorer_url` MUST start with `http://` or `https://` and contain no quotes, angle brackets or spaces (checked at start-up). The UI additionally renders a link only for `http(s)` URLs and opens it with `rel="noopener noreferrer"`, so a bad value can never become a script URL.

## 5. Checklist

- [x] Static UI with demo engine, no dependencies.
- [ ] Daemon serves `ui/` and implements `/api/v1`.
- [ ] Authentication and CSP in place.
- [ ] Field mapping verified against real daemon output.
- [ ] Accessibility review (contrast, keyboard use, screen reader fallback for the panes).

## 6. Open Questions

1. Whether write actions (for example disconnecting a worker) belong in a separate authenticated API.
2. Pagination and server-side sorting for more than 10,000 workers.
