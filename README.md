# Conduit v2

A **local-first, single-binary LLM gateway** with complete audit trail capability.

## What it does

- Drop-in OpenAI-compatible API endpoint (`POST /v1/chat/completions`)
- Routes requests to any LLM provider (OpenAI, Anthropic, and more)
- Records every request/response to a local append-only trace log
- Enforces per-key rate limits and records per-request usage/cost
- Provides a CLI (`conduitctl`) for trace inspection, usage reports, settings (trace on/off), and configuration
- Provides a desktop operator console (`conduit-ui`, Tauri + Svelte) for live monitor, traces, providers/OAuth, routes, keys, and usage

> **Note:** *conduitcli* is an informal/oral alias for the same tool — the binary and docs use **`conduitctl`**.

## Quick Start

```bash
# Start the daemon
conduitd --config conduit.toml

# Check status
conduitctl status

# View recent traces
conduitctl trace list

# List routes
conduitctl route list

# Pricing (LiteLLM standard map → local cache; offline until you sync)
conduitctl pricing list
conduitctl pricing sync          # fetch LiteLLM model_prices, convert, reload
conduitctl pricing reload        # re-read pricing.litellm.json + pricing.json

# OAuth login (Claude / Codex / Grok subscription accounts)
conduitctl oauth list
conduitctl oauth start claude --name "my-claude"
conduitctl oauth start codex
conduitctl oauth start grok   # device code flow — prints user_code
```

OAuth credentials are stored in the secret backend; tokens refresh automatically near expiry.
See [ARCHITECTURE.md](ARCHITECTURE.md#oauth-providers) for ports, kinds, and admin API.

### Operator UI

Desktop console lives in `conduit-ui/` (Tauri + Svelte). It talks to the same loopback admin API as `conduitctl` (`http://127.0.0.1:4001`). Design notes: [`docs/design/conduit-ui-rewrite.md`](docs/design/conduit-ui-rewrite.md).

```bash
cd conduit-ui && npm install && npm run tauri dev
```

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full architectural overview, including the L1-L7 pipeline, data storage design, security model, and codec contract.

## Development

### Build & quality gates

```bash
# Format
cargo fmt --all
cargo fmt --all --check

# Lints (CI treats warnings as errors)
cargo clippy --all-targets -- -D warnings

# Tests (prefer nextest if installed)
cargo test --workspace
cargo nextest run --workspace

# Targeted tests
cargo test -p conduitctl
cargo test -p conduitd
cargo test -p conduitd lagged_sse

# Advisories / licenses
cargo audit
cargo deny check
```

### Local run (daemon + CLI)

Default ports: gateway `4000`, admin `4001` (see `conduit.toml` / defaults).

```bash
# Build debug binaries
cargo build -p conduitd -p conduitctl

# Start daemon (config optional; falls back to defaults)
cargo run -p conduitd -- --config conduit.toml
# or with overrides:
cargo run -p conduitd -- --port 4000 --data-dir /tmp/conduit-dev --log info
# env equivalents: CONDUIT_CONFIG, CONDUIT_PORT, CONDUIT_DATA_DIR, CONDUIT_LOG, CONDUIT_LOG_FORMAT

# Verbose daemon logs
CONDUIT_LOG=debug cargo run -p conduitd -- --log-format pretty

# One-shot CLI against local admin API
cargo run -p conduitctl -- status
cargo run -p conduitctl -- --admin-addr http://127.0.0.1:4001 status
cargo run -p conduitctl -- trace list
cargo run -p conduitctl -- trace tail          # SSE live events (Ctrl+C to stop)
cargo run -p conduitctl -- --output json provider list
```

### Debug helpers

```bash
# Health (same as conduitctl status)
curl -s http://127.0.0.1:4001/health | jq .

# Admin traces (list / single / live SSE)
curl -s 'http://127.0.0.1:4001/admin/traces?limit=20' | jq .
curl -s http://127.0.0.1:4001/admin/traces/<id> | jq .
curl -N -H 'accept: text/event-stream' http://127.0.0.1:4001/admin/traces/stream

# Chat completions smoke (gateway; needs routes/providers configured)
curl -s http://127.0.0.1:4000/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}' | jq .

# Help surfaces
cargo run -p conduitd -- --help
cargo run -p conduitctl -- --help
cargo run -p conduitctl -- route get --help   # path arg is route id, not alias
cargo run -p conduitctl -- provider add --help
```

### Useful env vars

| Variable | Used by | Default / notes |
|----------|---------|-----------------|
| `CONDUIT_ADMIN_ADDR` | `conduitctl` | `http://127.0.0.1:4001` |
| `CONDUIT_OUTPUT` | `conduitctl` | `human` \| `json` |
| `CONDUIT_CONFIG` | `conduitd` | `conduit.toml` |
| `CONDUIT_PORT` | `conduitd` | gateway port override |
| `CONDUIT_DATA_DIR` | `conduitd` | `~/.conduit` |
| `CONDUIT_LOG` | `conduitd` | `info` (e.g. `debug`, `conduitd=trace`) |
| `CONDUIT_LOG_FORMAT` | `conduitd` | `pretty` \| `json` |
| `VITE_CONDUIT_ADMIN_URL` | `conduit-ui` | admin base for the desktop console |
| `RUST_LOG` | any | optional extra filter if used by tools |

## Project Structure

```
crates/
  conduit-ir/        Core types (zero deps, pure)
  conduit-codec/     OpenAI + Anthropic + Responses wire codecs
  conduit-router/    Pure function routing
  conduit-upstream/  HTTP client + SSE parsing
  conduit-oauth/     Claude/Codex/Grok OAuth + token refresh
  conduit-secret/    S1/S2 secret backends
  conduit-store/     SQLite config repos
  conduit-trace/     Append-only trace log + index
  conduit-quota/     Rate limit + usage record engine
  conduit-pipeline/  L1-L7 orchestration
  conduitd/          Daemon binary
  conduitctl/        CLI (`status`, `trace`, `provider`, `route`, `key`, `oauth`, …)
conduit-ui/          Tauri + Svelte operator console
docs/design/         Design docs
```
