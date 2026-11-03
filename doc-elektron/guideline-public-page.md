# Elektron Net: `elektron-net-photonpool` Public Pool Page Guideline

- **Version:** 0.1 (draft)
- **Date:** November 3, 2026
- **Audience:** Pool operators and developers
- **Reference implementation:** [`elektron-net-mempool`](https://github.com/kutlusoy/elektron-net-mempool) - block and transaction routes (`/block/<hash>`, `/tx/<txid>`) used for links - treat as ground truth for anything the doc references
- **See also:** [`guideline-admin-api-and-ui.md`](./guideline-admin-api-and-ui.md), [`guideline-payout-modes.md`](./guideline-payout-modes.md), [`../Quick-Install.md`](../Quick-Install.md)

---

## 1. What it is

The daemon can serve a public page for miners on its own listener (`[public]`, off by default). It uses the same terminal look as the operator UI and shows pool statistics, connection instructions, payout modes with fees, recent blocks and payouts, and lets a miner look up their own address (`account <address>`, or open `/#<address>`). The operator API and UI are never served on this listener.

## 2. Public API (read-only, `/public/v1`)

| Endpoint | Content |
|---|---|
| `GET /pool` | name, network, explorer URL, uptime, stratum host and ports with their default mode, modes with fee and availability, minimum payout, PPLNS window factor, hashrate (now, 1 min, 1 h, history), miners online and workers, blocks in 24 h and by status, node height, tip and network difficulty |
| `GET /blocks?limit=n` | height, hash, time, mode, status, **masked** finder (`be1q..abcd`), explorer link |
| `GET /payouts?limit=n` | time, txid, total amount, status |
| `GET /account/<address>` | hashrate and online workers, shares of the last hour and 24 h per mode, pending, payable and paid balance in leptons, last payouts. `400` for an address that is not valid for the configured network. |

No worker names, other accounts, IP addresses or operator data are exposed. The address lookup is public by design: anyone who knows an address sees its pool statistics, as on other pools.

## 3. Protection

- Per-client token bucket (`public.rate_per_min`, default 120). Over the limit the answer is `429` with `Retry-After: 10`.
- `X-Forwarded-For` is only trusted when `public.trust_proxy = true`; set it only behind your own reverse proxy (Caddy in the stack). Otherwise it is ignored, so clients cannot dodge the limit by spoofing it.
- Responses carry a strict `Content-Security-Policy` (`default-src 'none'`, scripts and styles only from the same origin), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and a short cache lifetime.
- Only `public.html`, `style.css`, `common.js` and `public.js` are served as static files (fixed list, no directory serving). The test suite checks that `index.html`, `app.js`, the operator API and path-traversal attempts are not reachable.

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
