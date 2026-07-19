# Conduit v2 Architecture

Conduit is a **local-first, single-binary LLM gateway**. It proxies requests to upstream LLM providers (OpenAI, Anthropic, and more) while tracking usage and cost for the operator console.

## Design Axioms

1. **Faithful Proxy**: Any field silently changed, dropped, or degraded during codec translation is a bug. All losses are recorded in `LossReport`.
2. **Local-first operator surface**: Configuration, usage, and secrets stay on the host running `conduitd`.
3. **Local-First, Single Binary**: No cloud dependencies. Offline-capable. Distribution = one signed binary (+ optional UI bundle). **Operator path is CLI + desktop console** (`conduitctl` + `conduit-ui`).

## Process Model

```
┌─────────────────────┐         ┌──────────────────────────────┐
│  conduit-ui         │         │  conduitctl                  │
│  (Tauri + Svelte)   │         │  one-shot operator CLI       │
│  operator console   │         │  (scripts / automation)      │
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

**Operator UX:** interactive console is **`conduit-ui`** (desktop). Scriptable operations use **`conduitctl`**. Oral alias *conduitcli* → formal name `conduitctl`.

### Tauri UI transport (Q5)

**Chosen form: A — Tauri is a window shell only.** The Svelte frontend talks to
`conduitd` over loopback HTTP (default `http://127.0.0.1:4001`), same as
`conduitctl`. There is no console IPC surface in `src-tauri`; Tauri commands are
not used for providers/routes/keys. Override the console base with
`VITE_CONDUIT_CONSOLE_URL` at build/dev time.

**Product priority (2026-07):** `conduit-ui` is the **operator console** (light product shell; providers/routes/keys/usage; multi-target route wizard; zero remote resources).

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

**L3 Router is a pure function**: `route(alias, table, attempt_no) → Decision`. Zero IO, zero locks. Fully unit-testable and deterministic.

Request **usage** is recorded directly from the pipeline into `usage_records`.

## Crate Dependency Graph

```
conduit-ir           (zero deps, pure types)
    ↑
conduit-router       (IR + pure routing logic)
conduit-codec        (IR + OpenAI/Anthropic/Responses wire translation)
conduit-secret       (IR + OS keychain / master password)
conduit-store        (IR + router + SQLite config repos)
conduit-quota        (IR + in-memory rate limit + usage record hook)
conduit-oauth        (Claude/Codex PKCE + Grok device code; credential refresh)
conduit-upstream     (IR + codec + reqwest HTTP client)
conduit-pipeline     (all of the above, L1-L6 orchestration)
    ↑
conduitd             (binary: axum server + console API + OAuth callbacks)
conduitctl           (binary: operator CLI)
```

## OAuth Providers

Conduit supports subscription-style OAuth for:

| Kind | Flow | Callback / UX | Upstream |
|------|------|---------------|----------|
| `claude-oauth` | Auth code + PKCE (Firefox_Auto TLS on token) | `http://localhost:54545/callback` | Messages `?beta=true` + **Chrome_Auto TLS** (wreq latest Chrome) + cloak/cch/tool-remap/signature |
| `codex-oauth` | Auth code + PKCE | `http://localhost:1455/auth/callback` | ChatGPT Codex `/responses` |
| `grok-oauth` | Device code (RFC 8628) | user_code + verification_uri | **Grok CLI chat-proxy** `https://cli-chat-proxy.grok.com/v1/responses` (not official `api.x.ai` chat) |

Credentials (access + refresh + expiry + account metadata) are stored as JSON in the secret backend under scope `upstream_key`. On each request, `CredentialResolver` refreshes tokens near expiry (singleflight) and injects provider-specific headers (e.g. `Chatgpt-Account-Id`, `anthropic-beta`).

Console API: `POST /console/oauth/{kind}/start`, `GET /console/oauth/sessions/{id}`, `POST .../cancel`, `POST /console/oauth/{provider_id}/refresh`. CLI: `conduitctl oauth start <claude|codex|grok>`.

## Data Storage

| Data | Storage | Rationale |
|------|---------|-----------|
| Config (providers, routes, keys) | SQLite (`config.db`) | Relational data, strong schema, ACID |
| Request usage | SQLite `usage_records` (per-request) | Spend and token accounting for the console |
| Secrets | OS Keychain (S1) or Master Password AES-256-GCM (S2) | No S3 machine-bound; silent downgrade eliminated |

## Security Model

**Secret backend tiers** (reported honestly by `SecurityLevel`; no tier is silently misrepresented):

| Level | Implementation | Trigger |
|-------|---------------|---------|
| S1 Hardware | macOS Keychain / Windows DPAPI / Linux libsecret | Available by default |
| S2 Master Password | Argon2id + AES-256-GCM encrypted files | User explicitly sets a master password |
| S0 Plaintext File | base64 file, mode 0600, **no encryption at rest** | The keychain-mirror path (see below) |

Downgrade from S1 → S2 always shows an explicit UI/CLI warning. The previous "machine-bound" S3 level is **deleted** — it provided false security (machine UUID is predictable).

**Keychain mirror (local-first tradeoff).** To avoid macOS Keychain ACL prompts
hanging the daemon, when S1 is available secrets are also mirrored to an
unencrypted file under `{data_dir}/secrets/` (base64, mode 0600), and **reads are
served from that file first**. This means the effective protection at rest is
filesystem permissions, not the hardware keychain — so `FileFallbackBackend`
reports `SecurityLevel::PlaintextFile` (not `Hardware`) and `build_backend`
surfaces a startup notice. This is an accepted local-first tradeoff; the level is
reported truthfully rather than claimed as hardware-backed.

All secrets are held as `secrecy::SecretVec<u8>` and zeroized on drop.

## Usage Ledger

Every completed request with non-zero tokens or cost is inserted into
`usage_records` from the pipeline (non-stream + stream finalize paths).
Aggregate views (`GET /console/usage/summary`) are SQL rollups over this table.

Hard monthly budget *caps* are not enforced; RPM rate limits remain.

## Pricing data sources

Cost estimation uses a layered pricing table (USD per million tokens):

| Layer | Source | Priority |
|-------|--------|----------|
| Embedded defaults | `DEFAULT_PRICING_JSON` in `conduit-store` | lowest |
| LiteLLM cache | `{data_dir}/pricing.litellm.json` | middle |
| Operator overrides | `{data_dir}/pricing.json` | highest |

Later layers win on `(provider_kind, model_id)`.

**Standard remote source:** LiteLLM
[`model_prices_and_context_window.json`](https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json)
(chat/completion only; per-token → per-MTok). Sync is **explicit** (local-first —
no fetch on boot):

- `POST /console/pricing/sync` (optional body `{ "url": "..." }`)
- `conduitctl pricing sync`
- UI: Pricing → “Sync LiteLLM”

Reload layers without network: `POST /console/pricing/reload` / `conduitctl pricing reload`.

## Codec Contract

Codec translation is governed by three test suites (all mandatory for every provider):
1. **Snapshot tests** (`insta`): recorded real responses, diff must be clean
2. **Property tests** (`proptest`): `IR → wire → IR` roundtrip invariance
3. **Integration tests** (`wiremock`): every finish_reason, tool_call, error path

Codec degradations (e.g., `ToolChoice::AnyOf → Required`) are never silent: they are added to in-memory `LossReport` on the codec path.
