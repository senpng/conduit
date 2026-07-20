# Conduit — Rust daemon (conduitd) + operator CLI (conduitctl).
# The optional conduit-ui desktop console is built separately with pnpm/tauri.

CARGO   ?= cargo
BINS     = -p conduitd -p conduitctl
RELDIR   = target/release

.DEFAULT_GOAL := build

.PHONY: help build release debug run-daemon run-ctl \
        test fmt fmt-check clippy deny check clean

help: ## List available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

build: release ## Alias for `release`

release: ## Build optimized conduitd + conduitctl (target/release)
	$(CARGO) build --release $(BINS)
	@echo "==> binaries: $(RELDIR)/conduitd  $(RELDIR)/conduitctl"

debug: ## Build unoptimized conduitd + conduitctl (target/debug)
	$(CARGO) build $(BINS)

run-daemon: ## Run the daemon (debug)
	$(CARGO) run -p conduitd

run-ctl: ## Run the CLI; pass args with ARGS="status"
	$(CARGO) run -p conduitctl -- $(ARGS)

# ── Quality gates (mirror README "Development" section) ──────────────────────

check: fmt-check clippy test deny ## Run all Rust quality gates

test: ## Run the workspace test suite
	$(CARGO) test --workspace

fmt: ## Format the workspace in place
	$(CARGO) fmt --all

fmt-check: ## Verify formatting without modifying files
	$(CARGO) fmt --all --check

clippy: ## Lint with warnings denied
	$(CARGO) clippy --workspace --all-targets -- -D warnings

deny: ## Check dependencies with cargo-deny
	$(CARGO) deny check

clean: ## Remove build artifacts
	$(CARGO) clean
