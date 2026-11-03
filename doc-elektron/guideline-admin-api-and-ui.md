# Elektron Net: `elektron-net-photonpool` Admin API and Terminal UI Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool developers and operators
- **Reference implementation:** [`elektron-net`](https://github.com/kutlusoy/elektron-net) - `src/interfaces/mining.h` (tip, template and block data the pool exposes) - treat as ground truth for anything the doc references
- **See also:** [`guideline-photon-architecture.md`](./guideline-photon-architecture.md), [`guideline-batch-attestation-rpc.md`](./guideline-batch-attestation-rpc.md)

---

## 1. Purpose

The operator UI (`ui/`) is a terminal-style single page: static HTML, CSS and JavaScript without dependencies and without a build step. The pool daemon serves it as static files and the page reads the read-only admin API below. The daemon MUST run fully without the UI.

## 2. UI behaviour

- Panes: pool, chain tip, allocator, miners, blocks and a console (TTY) with a command line.
- Commands: `help`, `status`, `tip`, `miners [n] [sort k] [grep s] [reset]`, `blocks`, `alloc`, `log [n]`, `watch`, `theme cyan|amber|green|magenta`, `clear`, `about`. Tab completes, Up and Down recall history, Ctrl+L clears.
- On load the page probes `/api/v1/summary`. If it is unreachable, or the page is opened from `file://`, it switches to a simulated engine and marks this with a blinking `DEMO DATA` tag in the title bar. Simulated numbers MUST NOT be shown without that tag. `?demo` forces demo mode.
- The layout follows the viewport (three columns on wide screens, stacked on phones). Animations stop with `prefers-reduced-motion`. Theme choice is kept in `localStorage` when available.
- The UI never sends commands that change pool state. All commands are local views.

## 3. Admin API (read-only, v1)

All responses are JSON. Authentication and binding are described in section 4.

| Endpoint | Content |
|---|---|
| `GET /api/v1/summary` | `pool {name, version, uptime, mode}`, `node {height, tip, tipTime, connected, transport, rttMs, p50, p95, samples, batchMs}`, `hashrate {now, m1, h1, hist[]}`, `miners {online, total, v1, v2, edge}`, `shares {perSec, ok, stale, dup, inv, total}`, `alloc {candidates, unitsPerSec, epochUse, overlap}`, `found` |
| `GET /api/v1/miners?limit=n` | array of `{name, proto, hashrate, diff, ok, stale, dup, seen, unit}` |
| `GET /api/v1/blocks?limit=n` | array of `{height, hash, time, worker, status}` with status `found`, `maturing` or `mature` |
| `GET /api/v1/events` | Server-Sent Events, each event a JSON object `{t, lvl, msg}` with `lvl` one of `info`, `share`, `job`, `warn`, `err`, `blk` |

Units: hashrate in hashes per second, times as Unix seconds (`tipTime`, `time`, `t`) or seconds (`uptime`, `seen`), ratios between 0 and 1. `tip>job` latencies (`p50`, `p95`) are milliseconds from tip change to the first job on the wire.

## 4. Security

1. The API and the static UI MUST bind to a loopback or management interface by default, never to the miner-facing ports.
2. Access MUST require authentication (token or mutual TLS) once bound beyond loopback.
3. Responses MUST be read-only and MUST NOT contain payout keys, RPC credentials, worker passwords or the node cookie.
4. The server SHOULD send a strict `Content-Security-Policy` (`default-src 'self'`) and `X-Content-Type-Options: nosniff`.
5. Worker names and other miner-supplied text are untrusted. The UI escapes all dynamic text before rendering.
6. The event stream and `limit` parameters MUST be bounded server-side.

## 5. Checklist

- [x] Static UI with demo engine, no dependencies.
- [ ] Daemon serves `ui/` and implements `/api/v1`.
- [ ] Authentication and CSP in place.
- [ ] Field mapping verified against real daemon output.
- [ ] Accessibility review (contrast, keyboard use, screen reader fallback for the panes).

## 6. Open Questions

1. Whether write actions (for example disconnecting a worker) belong in a separate authenticated API.
2. Pagination and server-side sorting for more than 10,000 workers.
