# Conduit Architecture

Conduit is a **local-first, single-binary LLM gateway**. It proxies requests to upstream LLM providers (OpenAI, Anthropic, and more) while tracking usage and cost for the operator console.

## Design Axioms

1. **Faithful Proxy**: Any field silently changed, dropped, or degraded during codec translation is a bug. All losses are recorded in `LossReport`.
2. **Local-first operator surface**: Configuration, usage, and secrets stay on the host running `conduitd`.
3. **Local-First, Single Binary**: No cloud dependencies. Offline-capable. Distribution = one signed binary (+ optional UI bundle). **Operator path is CLI/TUI + optional desktop console** (`conduitctl` + `conduit-ui`).

## Process Model

```
┌─────────────────────┐         ┌──────────────────────────────┐
│  conduit-ui         │         │  conduitctl                  │
│  (Tauri + Svelte)   │         │  one-shot CLI + interactive  │
│  optional desktop   │         │  TUI (primary terminal UX)   │
└─────────┬───────────┘         └──────────────┬───────────────┘
          │   loopback HTTP + console API         │
          └──────────────────┬──────────────────┘
                             ▼
                  ┌──────────────────────┐
                  │     conduitd         │
                  │  (axum gateway +     │
                  │   console API server)  │
                  └──────────────────────┘
```

**Operator UX:** primary interactive surface in the terminal is **`conduitctl tui`** (also launched when `conduitctl` is run with no subcommand on a TTY). One-shot subcommands remain for scripts/automation. **`conduit-ui`** is an optional desktop shell over the same console API. Oral alias *conduitcli* → formal name `conduitctl`.

### Tauri UI transport (Q5)

**Chosen form: A — Tauri is a window shell only.** The Svelte frontend talks to
`conduitd` over loopback HTTP (default `http://127.0.0.1:4001`), same as
`conduitctl`. There is no console IPC surface in `src-tauri`; Tauri commands are
not used for providers/routes/keys. Override the console base with
`VITE_CONDUIT_CONSOLE_URL` at build/dev time.

**Product priority (2026-07):** terminal operator console is **`conduitctl` TUI** (providers/routes/keys/usage/pricing/oauth; multi-target route wizard). `conduit-ui` remains an optional desktop shell over the same loopback console API (zero remote resources).

CSP (`src-tauri/tauri.conf.json`) allows both `http://127.0.0.1:4001` and
`http://localhost:4001` under `connect-src` for fetch/SSE. The console listener
applies a permissive `CorsLayer` so the Tauri/dev origin (`localhost:1420` /
`tauri.localhost`) can call the loopback console API; the client only sends
`Content-Type: application/json` when a request body is present.

## Pipeline Layers (L1-L6)

```
client ──► L1 Transport      (axum, tower middleware stack)
        ── L2 Ingress Filter (auth, rate-limit enforcement)
        ── L3 Router         (PURE function: alias → provider decision)
        ── L4 Codec          (IR ↔ wire format translation)
        ── L5 Upstream       (provider HTTP call, SSE parsing)
        ── L6 Egress Filter  (cost calculation, usage ledger)
```

**L3 Router is a pure function**: `route(alias, table, attempt_no, preferred_provider_id) → Decision`. Richer entry points layer on determinism knobs without side effects — `route_with_seed(…, seed)` for weighted load-balancing and `route_with_options(…, seed, skip_provider_ids, pool_cursors)` for cooldown-skip sets and pool round-robin cursors. Fully unit-testable.

**Affinity** is **session-scoped** (`session_id` + alias → `provider_id`), not downstream API key. Session id prefers Claude Code identity (`metadata.user_id` `_session_` / JSON `session_id`, then `X-Claude-Code-Session-Id`), then generic headers (`X-Session-ID`, …) and body fields; no session → no pin.

**Pool targets** (`pool_kind` / `pool_id`) schedule members with `pool_strategy`:
- `round_robin` (default) — stable cursor across requests
- `fill_first` — always first eligible in stable `provider_id` order  

Session pin is the base layer before pool mode. Explicit multi-target lists still use `fixed` / `fallback` / `weighted`.

Request **usage** is recorded directly from the pipeline into `usage_records`.

## Process Lifecycle & Observability

`conduitd` runs in the foreground by default, or **daemonizes** (`--daemon`:
fork, detach from the controlling terminal, redirect stdio). Shutdown is
**graceful**: SIGINT (Ctrl-C) and, on Unix, SIGTERM (what `kill <pid>` / a
daemon stop sends) trigger a drain so in-flight requests complete before exit.

Logging is `tracing`-based and configured under `[log]` in `conduit.toml` (each
key has an env override):

| Setting | Key / env | Notes |
|---------|-----------|-------|
| Level filter | `level` / `CONDUIT_LOG` | e.g. `info`, `debug,sqlx::query=off` |
| Format | `format` / `CONDUIT_LOG_FORMAT` | `pretty` or `json` |
| File sink | `to_file` / `CONDUIT_LOG_TO_FILE` | daily-rolling file vs stdout; rotation uses the **host local timezone** (`conduitd.log.YYYY-MM-DD`) |
| Log dir | `dir` / `CONDUIT_LOG_DIR` | defaults to `<data-dir>/logs` |

A single **`request_id`** is threaded end-to-end (ingress → router → codec →
upstream → egress) so one request's spans/logs correlate across all pipeline
layers.

## Crate Dependency Graph

```
conduit-ir           (zero deps, pure types)
    ↑
conduit-router       (IR + pure routing logic)
conduit-codec        (IR + OpenAI/Anthropic/Responses wire translation)
conduit-secret       (IR + master-password AES-256-GCM files)
conduit-store        (IR + router + SQLite config repos)
conduit-quota        (IR + in-memory rate limit + usage record hook)
conduit-oauth        (Claude/Codex PKCE + Grok device code; credential refresh)
conduit-upstream     (IR + codec + reqwest HTTP client)
conduit-pipeline     (all of the above, L1-L6 orchestration)
    ↑
conduitd             (binary: axum server + console API + OAuth callbacks)
conduitctl           (binary: operator CLI + TUI)
```

## OAuth Providers

Conduit supports subscription-style OAuth for:

| Kind | Flow | Callback / UX | Upstream |
|------|------|---------------|----------|
| `claude-oauth` | Auth code + PKCE (**Chrome_Auto TLS** on token + Messages) | `http://localhost:54545/callback` | Messages `?beta=true` + Chrome TLS + cloak (**auto**: non-`claude-cli` only)/cch/tool-remap/signature; refresh: 429 block + 3 retries |
| `codex-oauth` | Auth code + PKCE | `http://localhost:1455/auth/callback` | ChatGPT Codex `/responses`; refresh: 3 retries |
| `grok-oauth` | Device code (RFC 8628) | user_code + verification_uri | Chat defaults to **cli-chat-proxy**; set credential `using_api=true` for official `api.x.ai` |

Credentials (access + refresh + expiry + account metadata) are stored as JSON in the secret backend under scope `upstream_key`. On each request, `CredentialResolver` refreshes tokens near expiry (singleflight) and injects provider-specific headers (e.g. `Chatgpt-Account-Id`, `anthropic-beta`).

**Proxy** (OAuth login + token refresh): credential `proxy_url` → `CONDUIT_PROXY_URL` → `conduit.toml` `proxy_url` → `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` (SOCKS supported). Bypass: `NO_PROXY`.

**Codex multi-auth naming**: provider id derived from email + `chatgpt_plan_type` (+ account hash for `team`/`k12`), e.g. `codex-user_x.com-plus`, `codex-{hash}-user_x.com-team`.

**Upstream cooldown**: on HTTP 429 / `usage_limit_reached`, the provider is marked cooling (duration from `resets_in_seconds` / `resets_at` when present). Multi-target routes skip cooling providers. Console: `GET/DELETE /console/cooldowns`.

**OAuth callback ports**: IdP-registered fixed ports (Claude 54545, Codex 1455). New login stops prior Conduit callback listeners first; if another process still holds the port → `PortInUse` (cannot invent alternate ports without `redirect_uri_mismatch`). Grok device poll respects session cancel.

**Proxy**: configured proxy URL must apply; construction fails instead of falling back to direct connect.

**Quota snapshots**: best-effort last-seen rate-limit headers / 429 bodies per provider — `GET /console/quota-snapshots`, `POST .../{id}/refresh` (optional `clear_cooldown`), `POST .../refresh` (probe all OAuth). All OAuth kinds proactively probe remaining capacity (CodexBar-style, reusing stored access tokens):

| Kind | Probe | Remaining fields |
|------|-------|------------------|
| `claude-oauth` | `GET /api/oauth/usage` | session 5h + weekly 7d % |
| `codex-oauth` | `GET …/wham/usage` | weekly 7d % only (5h session removed) |
| `grok-oauth` | `POST grok.com …/GetGrokCreditsConfig` (gRPC-web) | monthly credits % (`mo`) |

Successful and error upstream responses also record `anthropic-ratelimit-*` / `x-ratelimit-*` / `retry-after`. TUI Providers tab shows a **REMAINING** column and detail meters (`u` re-probes).

Console API: `POST /console/oauth/{kind}/start`, `GET /console/oauth/sessions/{id}`, `POST /console/oauth/sessions/{id}/cancel`, `POST /console/oauth/{provider_id}/refresh`, `GET /console/oauth/providers`. CLI: `conduitctl oauth start <claude|codex|grok>`.

## Data Storage

| Data | Storage | Rationale |
|------|---------|-----------|
| Config (providers, routes, keys) | SQLite (`config.db`) | Relational data, strong schema, ACID |
| Request usage | SQLite `usage_records` (per-request) | Spend and token accounting for the console |
| Secrets | Master-password AES-256-GCM files under `{data_dir}/secrets/` | Single backend; no OS keychain ACL hangs |

## Security Model

**Secret backend** — only master-password encryption is supported:

| Level | Implementation | Key material |
|-------|---------------|--------------|
| MasterPassword | Argon2id (64 MiB, 3 iter) → AES-256-GCM | `CONDUIT_MASTER_PASSWORD` / `--master-password` |

On-disk layout per secret at `{data_dir}/secrets/{scope}/{id}.enc`:

```text
[ salt (16 B) ][ nonce (12 B) ][ ciphertext + GCM tag ]
```

Files are written atomically (tmp + rename) with mode `0600` on Unix.

If no master password is set, the daemon still starts (empty password KEK) and
logs a warning — suitable for local development only. Production deployments
must set a strong password; changing the password without re-encrypting
existing `.enc` files will make them unreadable.

All secrets are held as `secrecy::SecretVec<u8>` and zeroized on drop. A
process-local decrypted cache (`conduit-secret/cache.rs`) fronts the durable
backend to avoid re-running Argon2/AES on every request; entries stay in
`SecretVec` and are never written back to disk. OS keychain and plaintext
file-mirror backends were removed to keep a single, honest at-rest model.

## Usage Ledger

Every completed request with non-zero tokens or cost is inserted into
`usage_records` from the pipeline (non-stream + stream finalize paths). The
ledger is treated as a **billing-grade durable sink**: writes are committed on
the completion/finalize path so records survive restarts. Per-request rows also
carry throughput (**tokens/sec**) alongside token counts and cost. Aggregate
views (`GET /console/usage/summary`) are SQL rollups over this table.

Hard monthly budget *caps* are not enforced; RPM rate limits remain.

## Pricing data sources

Cost estimation uses a layered pricing table (USD per million tokens),
**file + memory only** (no SQLite `pricing` table):

| Layer | Source | Priority |
|-------|--------|----------|
| Embedded defaults | `DEFAULT_PRICING_JSON` in `conduit-store` | lowest |
| LiteLLM cache | `{data_dir}/pricing.litellm.json` | middle |
| Operator overrides | `{data_dir}/pricing.json` | highest |

Later layers win on `(provider_kind, model_id)`. **Pricing rows are price-only**
(no context-window fields).

**Operator overrides** (tokscale-style custom pricing): exact `(provider_kind, model_id)`
rows in `pricing.json`, USD **per million tokens**. Manage via:

- `GET/PUT /console/pricing/overrides`, `DELETE /console/pricing/overrides?provider_kind=…&model_id=…` (query params: model ids may contain `/`)
- `conduitctl pricing overrides|set|unset`
- TUI Pricing tab: `o` toggle overrides pane, `a`/`e`/`d` CRUD

**Standard remote source:** LiteLLM
[`model_prices_and_context_window.json`](https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json)
(chat/completion only; per-token → per-MTok). Sync is **explicit** (local-first —
no fetch on boot):

- `POST /console/pricing/sync` (optional body `{ "url": "..." }`)
- `conduitctl pricing sync`
- UI: Pricing → “Sync LiteLLM”

Reload layers without network: `POST /console/pricing/reload` / `conduitctl pricing reload`.

## Model limits (context window)

Token limits are a **separate** store from pricing (same LiteLLM JSON, second pipeline):

| Layer | Source | Priority |
|-------|--------|----------|
| LiteLLM limits cache | `{data_dir}/limits.litellm.json` | base |
| Operator overrides | `{data_dir}/limits.json` | highest |

Context window = LiteLLM **`max_input_tokens`** (not `max_tokens`, which is often
max output). `GET /v1/models` includes `context_window` / `context_length` when a
positive limit is known for the route target; otherwise those fields are omitted
(no invented default). LiteLLM pricing sync also refreshes the limits cache.

## Codec Contract

Codec translation is governed by three test suites (all mandatory for every provider):
1. **Snapshot tests** (`insta`): recorded real responses, diff must be clean
2. **Property tests** (`proptest`): `IR → wire → IR` roundtrip invariance
3. **Integration tests** (`wiremock`): every finish_reason, tool_call, error path

Codec degradations (e.g., `ToolChoice::AnyOf → Required`) are never silent: they are added to in-memory `LossReport` on the codec path.
