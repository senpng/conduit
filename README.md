# Conduit

Local-first LLM gateway with routing, protocol translation, and usage accounting.

Conduit exposes OpenAI-compatible (Chat Completions and Responses) and Anthropic Messages endpoints, forwards requests to configured providers, and keeps operational data on your machine. It consists of a Rust daemon (`conduitd`), a scriptable CLI (`conduitctl`), and an optional Tauri + Svelte desktop console (`conduit-ui`).

> [!WARNING]
> Conduit is pre-release software under active development. Configuration and storage formats may change without migration support. There are no official binary releases yet; build from source for development and evaluation.

## Why Conduit?

- **Local-first operation**: configuration, secrets, and usage records remain on the host running Conduit.
- **Compatible ingress APIs**: supports OpenAI Chat Completions, OpenAI Responses, and native Anthropic Messages requests, including streaming.
- **Multi-provider routing**: fixed, weighted, and ordered fallback strategies with retry handling.
- **Faithful protocol translation**: codec losses are explicit (`LossReport`) instead of being silently discarded.
- **Usage ledger**: per-request tokens and cost for operator spend queries.
- **Operator tooling**: inspect and configure the gateway through a desktop console or automate common operations with `conduitctl`.
- **OAuth provider support**: includes Claude, Codex, and Grok subscription-account flows in addition to API-key providers.

## Project Status

The core gateway path, provider and route management, streaming, usage accounting, pricing, and operator console are implemented. The project is not yet production-ready: release automation, versioned database migrations, end-to-end test coverage, and several community files are still in progress.

## Architecture

```mermaid
flowchart LR
  subgraph Clients[Clients and operator tools]
    direction TB
    OpenAI[OpenAI-compatible clients]
    Anthropic[Anthropic clients]
    UI[conduit-ui]
    CLI[conduitctl]
  end

  subgraph Daemon[conduitd]
    direction TB
    Gateway[Gateway API<br/>127.0.0.1:4000]
    Pipeline[L1-L6 pipeline<br/>Auth · Route · Codec · Upstream]
    Console[Console API<br/>127.0.0.1:4001]
    Store[Configuration and usage<br/>SQLite]

    Gateway --> Pipeline
    Pipeline --> Store
    Console --> Store
    Console -. Reload routes .-> Pipeline
  end

  Providers[LLM providers<br/>API keys or OAuth]

  OpenAI -->|POST /v1/chat/completions| Gateway
  OpenAI -->|POST /v1/responses| Gateway
  Anthropic -->|POST /v1/messages| Gateway
  UI --> Console
  CLI --> Console
  Pipeline --> Providers
```

Requests pass through a layered pipeline: transport, ingress filters, routing, codec translation, upstream transport, and egress accounting (usage/cost ledger). The routing decision is pure and deterministic; configuration and usage records use SQLite.

For crate boundaries, storage details, security trade-offs, and codec contracts, read [ARCHITECTURE.md](ARCHITECTURE.md).

## Supported APIs and Providers

### Ingress APIs

| API | Endpoint | Streaming | Notes |
| --- | --- | --- | --- |
| OpenAI Chat Completions | `POST /v1/chat/completions` | Yes | Classic chat `messages` shape |
| OpenAI Responses | `POST /v1/responses` | Yes | First-class ingress; local `previous_response_id` continuation when `store` is enabled |
| Anthropic Messages | `POST /v1/messages` | Yes | Native Anthropic wire format |
| OpenAI Models | `GET /v1/models` | N/A | Lists configured route aliases |

Clients can speak Chat Completions or Responses on the gateway; Conduit translates to the selected upstream protocol (including Responses-native providers such as Codex and Grok chat-proxy).

### Provider kinds

| Kind | Authentication | Upstream protocol | Notes |
| --- | --- | --- | --- |
| `openai` | API key | Chat Completions | OpenAI-compatible chat upstreams |
| `anthropic` | API key | Messages | Anthropic Messages upstreams |
| `claude-oauth` | OAuth + PKCE | Messages | Claude subscription account |
| `codex-oauth` | OAuth + PKCE | Responses | ChatGPT Codex `/responses` |
| `grok-oauth` | OAuth device flow | Responses | Grok CLI chat-proxy `/v1/responses` |

Provider compatibility is evolving; treat codec edge cases and OAuth flows as best-effort until a release is cut.

## Getting Started

### Prerequisites

- Rust stable toolchain with Cargo
- Node.js and pnpm for the optional desktop console
- Tauri 2 platform prerequisites for your operating system if you run the desktop console

The repository currently builds with Rust 1.94 and pnpm 10. Earlier versions may work but are not tested here.

### 1. Build the daemon and CLI

```bash
git clone <repository-url>
cd conduit
cargo build -p conduitd -p conduitctl
```

This repository does not currently declare a public Git remote, so replace `<repository-url>` with the URL of your fork or checkout source.

### 2. Start the daemon

```bash
cargo run -p conduitd
```

If `conduit.toml` is absent, Conduit uses its built-in defaults:

- gateway: `http://127.0.0.1:4000`
- console API: `http://127.0.0.1:4001`
- data directory: `~/.conduit`
- tracing: enabled

Confirm that the daemon is available:

```bash
cargo run -p conduitctl -- status
```

### 3. Start the operator console

In a second terminal:

```bash
cd conduit-ui
pnpm install --frozen-lockfile
pnpm tauri dev
```

Use **Providers** to add an API-key provider or start an OAuth login. Then use **Routes** to map the model name sent by clients, such as `gpt-4o`, to an upstream provider and model.

Each route target can also define **Request overrides** as a JSON object. These
static fields are applied only when that target is selected, after Conduit has
encoded the upstream protocol. This is useful for provider-specific options
that VS Code and other clients cannot place in their request body. For example,
set the following on a `codex-oauth` target for `gpt-5.6-terra` to enable its
Fast service tier:

```json
{"service_tier":"priority"}
```

Conduit does not allow overrides to replace gateway-controlled fields: `model`,
`stream`, `store`, `input`, and `messages`.

The console connects to `http://127.0.0.1:4001` by default. Set `VITE_CONDUIT_CONSOLE_URL` before starting it to use another console address.

### 4. Send a request

After configuring a provider and a route, send an OpenAI-compatible request. Use the downstream key created in the **Keys** view as a bearer token.

**Chat Completions:**

```bash
curl http://127.0.0.1:4000/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer <conduit-key>' \
  -d '{
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "Hello from Conduit"}]
  }'
```

**Responses** (same route alias; useful for Codex / Grok clients and Responses-native SDKs):

```bash
curl http://127.0.0.1:4000/v1/responses \
  -H 'content-type: application/json' \
  -H 'authorization: Bearer <conduit-key>' \
  -d '{
    "model": "gpt-4o",
    "input": "Hello from Conduit",
    "store": true
  }'
```

With `"store": true`, subsequent turns may pass `previous_response_id` and Conduit will expand the continuation from local state when needed.

Inspect usage and spend in the desktop console or with the CLI:

```bash
cargo run -p conduitctl -- usage summary
cargo run -p conduitctl -- usage list
```

Run `cargo run -p conduitctl -- --help` or append `--help` to any subcommand for the complete CLI reference.

## Configuration

Conduit works without a configuration file. To override the defaults, create `conduit.toml` in the working directory:

```toml
[gateway]
port = 4000
console_port = 4001

[security]
backend = "keychain"
```

### Environment variables

| Variable | Component | Default | Purpose |
| --- | --- | --- | --- |
| `CONDUIT_CONFIG` | `conduitd` | `conduit.toml` | Configuration file path |
| `CONDUIT_PORT` | `conduitd` | `4000` | Gateway port override |
| `CONDUIT_DATA_DIR` | `conduitd` | `~/.conduit` | Local state directory |
| `CONDUIT_LOG` | `conduitd` | `info` | Log filter, for example `debug` |
| `CONDUIT_LOG_FORMAT` | `conduitd` | `pretty` | `pretty` or `json` |
| `CONDUIT_CONSOLE_ADDR` | `conduitctl` | `http://127.0.0.1:4001` | Console API base URL |
| `CONDUIT_OUTPUT` | `conduitctl` | `human` | `human` or `json` where supported |
| `VITE_CONDUIT_CONSOLE_URL` | `conduit-ui` | `http://127.0.0.1:4001` | Desktop console API base URL |

> [!IMPORTANT]
> The console API is an operator interface and currently has no independent authentication layer. Keep it bound to a trusted loopback interface and do not expose port `4001` to untrusted networks.

## Data and Security

By default, Conduit stores its state under `~/.conduit`:

| Data | Storage |
| --- | --- |
| Providers, routes, keys | SQLite |
| Usage records | SQLite |
| Provider secrets | Selected secret backend plus the documented local mirror behavior |

Secret handling currently makes an explicit local-first trade-off: the keychain-backed path mirrors secrets to a mode-`0600` file so the daemon can operate without interactive keychain prompts. Treat the data directory as sensitive and read the full [security model](ARCHITECTURE.md#security-model) before evaluating Conduit for production use.

## Development

Run the Rust quality gates from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

Run the desktop console checks from `conduit-ui`:

```bash
pnpm install --frozen-lockfile
pnpm check
pnpm test
pnpm build
```

Useful focused commands:

```bash
cargo test -p conduit-codec
cargo test -p conduit-router
cargo test -p conduitd
cargo run -p conduitd -- --help
cargo run -p conduitctl -- --help
```

### Repository layout

```text
crates/
  conduit-ir/        Canonical request, response, usage, and span/content types
  conduit-codec/     OpenAI, Anthropic, and Responses wire codecs
  conduit-router/    Pure routing decisions and retry policies
  conduit-upstream/  Provider HTTP clients and SSE handling
  conduit-oauth/     Claude, Codex, and Grok OAuth flows
  conduit-secret/    Secret backend implementations
  conduit-store/     SQLite configuration, pricing, and usage repositories
  conduit-quota/     Rate limiting and usage hooks
  conduit-pipeline/  End-to-end request orchestration
  conduitd/          Gateway daemon and console API
  conduitctl/        Operator CLI
conduit-ui/          Tauri 2 + Svelte 5 desktop console
```

## Contributing

Contributions are welcome while the project is taking shape. Before opening a change:

1. Keep changes focused and preserve Conduit's faithful-proxy guarantees.
2. Add or update tests for behavior changes.
3. Run the Rust and UI quality gates relevant to your change.
4. Explain user-visible behavior, compatibility impact, and storage changes in the pull request.

A dedicated contribution guide, issue templates, and pull request template have not been added yet.

## Security

Do not report suspected vulnerabilities in a public issue. This repository does not yet publish a private disclosure address or response SLA. Until a security policy is added, contact the repository owner through a private channel available on the hosting platform.

## License

No license has been selected or included yet. Until a `LICENSE` file is added, copyright law reserves all rights and the source is **not** licensed for redistribution or modification. Do not describe this repository as open source solely because its source is visible.
