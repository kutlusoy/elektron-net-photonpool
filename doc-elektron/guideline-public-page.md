# Elektron Net: `elektron-net-photonpool` Public Pool Page Guideline

- **Version:** 0.2 (single-page shell, device list, stored history)
- **Date:** November 5, 2026
- **Audience:** Pool operators and developers
- **Reference implementation:** [`elektron-net-mempool`](https://github.com/kutlusoy/elektron-net-mempool) - block and transaction routes (`/block/<hash>`, `/tx/<txid>`) used for links - treat as ground truth for anything the doc references
- **See also:** [`guideline-admin-api-and-ui.md`](./guideline-admin-api-and-ui.md), [`guideline-payout-modes.md`](./guideline-payout-modes.md), [`../Quick-Install.md`](../Quick-Install.md)

---

## 1. What it is

The daemon can serve a public page for miners on its own listener (`[public]`, off by default). It uses the same terminal look as the operator UI and shows pool statistics, connection instructions, payout modes with fees, recent blocks and payouts, and lets a miner look up their own address (`account <address>`, or open `/#<address>`). The operator API and UI are never served on this listener.

## 1.0 One page, several views

The public pages are one single-page app (`index.html` with `spa.js`, the same shell on both listeners): the pool view (`/`), the engine view (`/engine`: chain tip, allocator, share quality and a sanitized event feed polled from `/public/v1/events?since=N`, the same numbers the operator sees minus anything about miners or the node) and the miner view (`/miner`, `/miner/<address>`, also `/miner.html#<address>`) are swapped in place with a short view transition and no reload, so the title bar with the **music player** and the clock stays and **the music keeps playing** when a visitor changes the page. A visitor who opens a path directly or reloads starts without music at first (browsers allow sound only after a click or key press); if the music was playing, it **resumes at the first click or key press anywhere on the page**, on the same track and at the same position (the browser remembers this locally; pause and stop are remembered too, so nothing starts that the visitor switched off). While it waits the player shows "click anywhere to resume". The browser's back button and the command `miner <address>` in the terminal work as navigation. There is no build step: plain JavaScript files, no framework.

The engine view has a **live scene** (`scene3d.js`, Three.js r149 vendored in `ui/vendor/` with its MIT notice, loaded only when the ENGINE view opens; `flow.js` is the light 2D fallback without WebGL). It shows the node with rays, sparkles and a stream of photon fire at the horizon, the attestation ring the candidates pass, the block template assembled from the candidates, the allocator units filling with the epoch use, the share check gate where accepted shares reach the node and stale or invalid ones fall, the miners and, when Photon is on, the connected gateways with their channels. The miners are shown in three fields by payout mode (SOLO, PPLNS, PPS, each in its own color, with the live count and a note when PPS is paused); a found block flies from the field of the mode that found it. The view can be turned by dragging (limited, never below the floor), zoomed with the wheel between 0.7x and 1.5x and reset with a double click; hovering a station shows its real numbers. The scene starts only when the ENGINE view is opened (the camera glides in) and stops when it is left. It has three quality levels: small pane = level 0 (12 fps, few particles, no bloom); maximized = level 2 (30 fps, bloom, streak trails, up to 600 fire particles); if the frame rate cannot keep up it steps down by itself to level 1 and 0 and does not step up again until the page is reloaded. The bloom is a small own pipeline (bright pass at a quarter of the size, two blur rounds, added back), not the three.js post-processing add-ons. Everything is driven by the aggregate counters of `/public/v1/engine` (`net`, `shares`, `alloc`, `photon`); the particle numbers grow logarithmically with the real rates and stay bounded (about 30 frames per second, instanced meshes, paused while hidden). The `photon` object of the public engine data holds only counts (gateways, channels, shares ok and rejected, checks); the listen address, the key and the timings stay in the admin API.

The pool view also shows the **device table** and the **pool history**:

- **Devices:** the miners listed by the name they send when they connect (user agent), for example Bitaxe, NerdMiner, NerdQAxe+, cpuminer, with the number of working connections, their hash rate and the best share difficulty ever seen from that type. Known open source miners are linked to their project and marked with a star (open source software and hardware); unknown names are listed as they are. The list of known miners is in `src/devices.rs`.
- **History:** a hash rate chart with the ranges 1 h, 24 h, 7 d, 30 d and all, read from the statistics that the pool stores (below). A switch changes between the whole pool and **one line per device type** (the eight types with the most hash rate); the maximized pane adds the hash rate and the working connections by device type. The maximized pane adds miners online, the share of valid shares, the network difficulty and how long the history is kept.

## 1.1 The miner page

`/miner.html#<address>` (also `/miner`) is the personal overview of one payout address: account and status, balance with a progress bar to the next payout (`payout.min_payout_leptons`, default 100,000), 24 h hashrate, workers, pending credits per block (`matures in N` blocks, `payable`), payout history with transaction links, and the blocks found. It is reached from the pool page (`MINER PAGE`, or the command `miner <address>`), pasted addresses are remembered in the browser only. It uses nothing but the account endpoint below.

## 2. Public API (read-only, `/public/v1`)

| Endpoint | Content |
|---|---|
| `GET /pool` | name, network, explorer URL, uptime, stratum host and ports with their default mode, modes with fee and availability, minimum payout, PPLNS window factor, hashrate (now, 1 min, 1 h, history), miners online and workers, blocks in 24 h and by status, node height, tip and network difficulty |
| `GET /blocks?limit=n` | height, hash, time, mode, status, **masked** finder (`be1q..abcd`), explorer link |
| `GET /payouts?limit=n` | time, txid, total amount, status |
| `GET /devices` | `devices[]` with `name`, project `url` (null for unknown types), `openSource`, `working` connections, `hashrate` and `best` share difficulty per device type, sorted by hash rate; no worker names, no agent strings other than the type name |
| `GET /history/devices?range=1h\|24h\|7d\|30d\|all` | the same by device type: `devices[]` with `key`, `name` and `points` of `[time, hash rate, working connections]` for the eight types with the most hash rate; stored once per `ledger.device_history_minutes` (default 5) |
| `GET /history?range=1h\|24h\|7d\|30d\|all` | stored pool statistics: `points` of `[time, hash rate, miners, ok, stale, dup, network difficulty]` in slots of `stepMinutes`, and `keepDays`; the answer is cached for 20 s |
| `GET /account/<address>` | hashrate, online workers (`workerList` with mode, hashrate, difficulty, accepted, stale, invalid, last seen), shares of the last hour and 24 h per mode, pending, payable and paid balance in leptons, minimum payout, last payouts, PPLNS credits per block with the blocks until maturity (`credits`), blocks found by the address, and a 24 h hashrate history in 10 minute slots. `400` for an address that is not valid for the configured network. |

No worker names, other accounts, IP addresses or operator data are exposed. The address lookup is public by design: anyone who knows an address sees its pool statistics, as on other pools.

## 3. Protection

- Per-client token bucket (`public.rate_per_min`, default 120) for the data API under `/public/v1/`. Over the limit the answer is `429` with `Retry-After: 10`. Static files and the music stream are not counted (a page view fetches about ten files).
- `X-Forwarded-For` is only trusted when `public.trust_proxy = true`; set it only behind your own reverse proxy (Caddy in the stack). Otherwise it is ignored, so clients cannot dodge the limit by spoofing it.
- Responses carry a strict `Content-Security-Policy` (`default-src 'none'`, scripts and styles only from the same origin), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and a short cache lifetime.
- Only `index.html` (also for `/engine`, `/ops`, `/miner`, `/miner/<address>` and `/miner.html`), `style.css`, `common.js`, `spa.js`, `charts.js`, `stats.js`, `music.js`, `depth.js`, `public.js`, `engine.js` and `miner.js` are served as static files (fixed list, no directory serving), plus the audio files of the music folder (`/music/`, audio extensions only). The test suite checks that `ops.js`, `app.js`, the operator API and path-traversal attempts are not reachable.

## 4. Configuration

```toml
[public]
enabled = true
listen = "127.0.0.1:8082"      # put a reverse proxy with TLS in front, or bind publicly yourself
name = "My Photon Pool"
stratum_host = "pool.example.org"   # shown in the connection instructions
trust_proxy = true             # only behind a trusted reverse proxy
rate_per_min = 120

[api]
explorer_url = "https://mempool.elektron-net.org"   # links for blocks and payout transactions
```

## 5. Tests

`test/step12_public_page.py` runs the daemon against a regtest node and checks the endpoints, the masking, the account lookup (valid, malformed, wrong checksum), the security headers, the static file whitelist and the rate limiter, including `X-Forwarded-For` spoofing.

## 6. Checklist

- [x] Public API and page implemented and tested.
- [ ] TLS and public domain (reverse proxy, see the stack integration).
- [ ] Accessibility review of the terminal layout.

## 7. Open Questions

1. Whether the account lookup should offer an opt-out for privacy-minded miners.
2. Whether a per-address request limit is needed in addition to the per-client limit.
