/**
 * Pure HTTP console client for conduitd loopback console API.
 *
 * Injectable `fetch` for unit tests. Default base: http://127.0.0.1:4001
 * (override via VITE_CONDUIT_CONSOLE_URL or setConsoleBase).
 */

export type FetchLike = typeof fetch;

const DEFAULT_CONSOLE_BASE = "http://127.0.0.1:4001";

let consoleBase =
  (typeof import.meta !== "undefined" &&
    (import.meta as ImportMeta & { env?: { VITE_CONDUIT_CONSOLE_URL?: string } }).env
      ?.VITE_CONDUIT_CONSOLE_URL) ||
  DEFAULT_CONSOLE_BASE;

export function getConsoleBase(): string {
  return consoleBase.replace(/\/$/, "");
}

export function setConsoleBase(url: string): void {
  consoleBase = url.replace(/\/$/, "");
}

export function consoleUrl(path: string, base = getConsoleBase()): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  return `${base}${p}`;
}

export class ConsoleClientError extends Error {
  constructor(
    public readonly status: number,
    public readonly path: string,
    public readonly body: string,
  ) {
    super(`${status} ${path}: ${body}`);
    this.name = "ConsoleClientError";
  }
}

export type RequestOptions = RequestInit & {
  /** Override fetch implementation (tests). */
  fetchImpl?: FetchLike;
  /** When true, do not parse JSON body (204 etc.). */
  empty?: boolean;
};

/**
 * Build request headers for the console client.
 * Only sets `Content-Type: application/json` when a body is present so simple
 * GET/DELETE avoid unnecessary CORS preflight from the Tauri/dev origin.
 */
export function buildConsoleHeaders(
  init: RequestInit = {},
): Headers {
  const headers = new Headers(init.headers);
  const hasBody = init.body != null && init.body !== "";
  if (hasBody && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  return headers;
}

export async function consoleRequest<T>(
  path: string,
  init: RequestOptions = {},
): Promise<T> {
  const { fetchImpl = fetch, empty = false, headers, ...rest } = init;
  const url = consoleUrl(path);
  const mergedHeaders = buildConsoleHeaders({
    ...rest,
    headers,
  });
  const res = await fetchImpl(url, {
    ...rest,
    headers: mergedHeaders,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new ConsoleClientError(res.status, path, text);
  }
  if (empty || res.status === 204) {
    return undefined as T;
  }
  const text = await res.text();
  if (!text) {
    return undefined as T;
  }
  return JSON.parse(text) as T;
}

// ── Types (daemon contract) ──────────────────────────────────────────────────

export interface Provider {
  id: string;
  name: string;
  kind: string;
  base_url: string;
  upstream_key_ref: string;
  created_at: string;
  updated_at: string;
}

export interface CreateProviderBody {
  name: string;
  kind: string;
  base_url: string;
  /** Daemon field name is `api_key`, not `secret`. */
  api_key?: string;
}

export interface Route {
  id: string;
  match_alias: string;
  strategy: string;
  targets_json: string;
  retry_policy_json: string;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreateRouteBody {
  match_alias: string;
  strategy: string;
  /** JSON array of RouteTarget — daemon expects object/array, not a string. */
  targets: unknown;
  retry_policy?: unknown;
}

export interface DownstreamKey {
  id: string;
  name: string;
  model_whitelist?: unknown;
  rate_limit_rpm: number | null;
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

/** Daemon KeyCreateResponse uses `key`, not `raw_key`. */
export interface CreateKeyResponse {
  id: string;
  key: string;
  name: string;
  model_whitelist: string[];
  rate_limit_rpm: number | null;
  created_at: string;
}

export interface UsageSummaryEntry {
  downstream_key_id: string;
  request_count: number;
  total_usd: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
}

/** UTC calendar day rollup for the summary period. */
export interface UsageDayEntry {
  day: string;
  request_count: number;
  total_usd: number;
  total_tokens: number;
}

/** Model / alias rollup for the summary period. */
export interface UsageModelEntry {
  label: string;
  provider_kind: string | null;
  request_count: number;
  total_usd: number;
  total_tokens: number;
}

export interface UsageSummaryResponse {
  period: string;
  total_usd: number;
  request_count: number;
  /** Present when summary was scoped with `key_id`. */
  key_id?: string | null;
  entries: UsageSummaryEntry[];
  /** Period-accurate daily spend (not limited to recent-N records). */
  by_day?: UsageDayEntry[];
  /** Period-accurate model/alias spend. */
  by_model?: UsageModelEntry[];
}

export interface UsageRecord {
  id: string;
  ts: string;
  request_id: string;
  downstream_key_id: string | null;
  alias: string | null;
  provider_id: string | null;
  provider_kind: string | null;
  model_id: string | null;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  reasoning_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  cost_usd: number;
  stream: boolean;
}

export interface UsageListResponse {
  entries: UsageRecord[];
}

export interface PricingRow {
  provider_kind: string;
  model_id: string;
  input_per_mtok: number;
  output_per_mtok: number;
  cache_read_per_mtok: number | null;
  cache_write_per_mtok: number | null;
  reasoning_per_mtok: number | null;
  effective_from: string;
}

export interface HealthResponse {
  status: string;
  version: string;
}


export interface OAuthProviderMeta {
  kind: string;
  display_name: string;
  flow: string;
  default_base_url: string;
  callback_port: number | null;
}

export interface OAuthSession {
  session_id: string;
  kind: string;
  status: "pending" | "completed" | "error" | "cancelled";
  auth_url?: string | null;
  user_code?: string | null;
  verification_uri?: string | null;
  verification_uri_complete?: string | null;
  expires_in?: number | null;
  provider_id?: string | null;
  email?: string | null;
  error?: string | null;
}

// ── Domain clients ───────────────────────────────────────────────────────────

export function createConsoleApi(fetchImpl?: FetchLike) {
  const opts = (init?: RequestOptions): RequestOptions => ({
    ...init,
    fetchImpl: init?.fetchImpl ?? fetchImpl,
  });

  return {
    health: {
      check: () => consoleRequest<HealthResponse>("/health", opts()),
    },


    providers: {
      list: () => consoleRequest<Provider[]>("/console/providers", opts()),
      get: (id: string) =>
        consoleRequest<Provider>(`/console/providers/${id}`, opts()),
      create: (body: CreateProviderBody) =>
        consoleRequest<Provider>("/console/providers", {
          ...opts(),
          method: "POST",
          body: JSON.stringify(body),
        }),
      update: (
        id: string,
        body: { name?: string; base_url?: string },
      ) =>
        consoleRequest<Provider>(`/console/providers/${id}`, {
          ...opts(),
          method: "PUT",
          body: JSON.stringify(body),
        }),
      delete: (id: string) =>
        consoleRequest<void>(`/console/providers/${id}`, {
          ...opts({ empty: true }),
          method: "DELETE",
        }),
      /** Daemon expects `{ api_key: string }`. */
      setSecret: (id: string, api_key: string) =>
        consoleRequest<void>(`/console/providers/${id}/secret`, {
          ...opts({ empty: true }),
          method: "PUT",
          body: JSON.stringify({ api_key }),
        }),
    },

    routes: {
      list: () => consoleRequest<Route[]>("/console/routes", opts()),
      get: (id: string) => consoleRequest<Route>(`/console/routes/${id}`, opts()),
      create: (body: CreateRouteBody) =>
        consoleRequest<Route>("/console/routes", {
          ...opts(),
          method: "POST",
          body: JSON.stringify(body),
        }),
      update: (
        id: string,
        body: {
          match_alias?: string;
          strategy?: string;
          targets?: unknown;
          retry_policy?: unknown;
          enabled?: boolean;
        },
      ) =>
        consoleRequest<Route>(`/console/routes/${id}`, {
          ...opts(),
          method: "PUT",
          body: JSON.stringify(body),
        }),
      delete: (id: string) =>
        consoleRequest<void>(`/console/routes/${id}`, {
          ...opts({ empty: true }),
          method: "DELETE",
        }),
    },

    keys: {
      list: () => consoleRequest<DownstreamKey[]>("/console/keys", opts()),
      get: (id: string) =>
        consoleRequest<DownstreamKey>(`/console/keys/${id}`, opts()),
      create: (body: {
        name: string;
        model_whitelist?: string[];
        rate_limit_rpm?: number;
      }) =>
        consoleRequest<CreateKeyResponse>("/console/keys", {
          ...opts(),
          method: "POST",
          body: JSON.stringify(body),
        }),
      update: (
        id: string,
        body: {
          name?: string;
          model_whitelist?: string[];
          rate_limit_rpm?: number | null;
          enabled?: boolean;
        },
      ) =>
        consoleRequest<DownstreamKey>(`/console/keys/${id}`, {
          ...opts(),
          method: "PUT",
          body: JSON.stringify(body),
        }),
      delete: (id: string) =>
        consoleRequest<void>(`/console/keys/${id}`, {
          ...opts({ empty: true }),
          method: "DELETE",
        }),
    },

    usage: {
      /**
       * Period rollup: by key + by day + by model.
       * Optional `keyId` scopes day/model (and reported totals) to one key.
       */
      summary: async (period?: string, keyId?: string) => {
        const params = new URLSearchParams();
        if (period) params.set("period", period);
        if (keyId) params.set("key_id", keyId);
        const q = params.toString() ? `?${params}` : "";
        return consoleRequest<UsageSummaryResponse>(`/console/usage/summary${q}`, opts());
      },
      list: async (limit = 50, keyId?: string) => {
        let path = `/console/usage?limit=${encodeURIComponent(String(limit))}`;
        if (keyId) path += `&key_id=${encodeURIComponent(keyId)}`;
        return consoleRequest<UsageListResponse>(path, opts());
      },
    },

    pricing: {
      list: () => consoleRequest<PricingRow[]>("/console/pricing", opts()),
      reload: () =>
        consoleRequest<{ status: string }>("/console/pricing/reload", {
          ...opts(),
          method: "POST",
        }),
      /** Fetch LiteLLM cost map, convert, cache as pricing.litellm.json, reload. */
      sync: (url?: string) =>
        consoleRequest<{
          status: string;
          source?: string;
          url?: string;
          sync_date?: string;
          source_models?: number;
          skipped?: number;
          total_rows?: number;
        }>("/console/pricing/sync", {
          ...opts(),
          method: "POST",
          body: url ? JSON.stringify({ url }) : JSON.stringify({}),
        }),
    },


    oauth: {
      listProviders: () =>
        consoleRequest<OAuthProviderMeta[]>("/console/oauth/providers", opts()),
      start: (
        kind: string,
        body: { name?: string; provider_id?: string } = {},
      ) =>
        consoleRequest<OAuthSession>(`/console/oauth/${encodeURIComponent(kind)}/start`, {
          ...opts(),
          method: "POST",
          body: JSON.stringify(body),
        }),
      session: (id: string) =>
        consoleRequest<OAuthSession>(
          `/console/oauth/sessions/${encodeURIComponent(id)}`,
          opts(),
        ),
      cancel: (id: string) =>
        consoleRequest<{ ok: boolean }>(
          `/console/oauth/sessions/${encodeURIComponent(id)}/cancel`,
          {
            ...opts(),
            method: "POST",
          },
        ),
      refresh: (providerId: string) =>
        consoleRequest<unknown>(
          `/console/oauth/${encodeURIComponent(providerId)}/refresh`,
          {
            ...opts(),
            method: "POST",
          },
        ),
    },
  };
}

/** Default singleton wired to global fetch. */
export const api = createConsoleApi();

// Back-compat named exports used by views
export const health = api.health;
export const providers = api.providers;
export const routes = api.routes;
export const keys = api.keys;
export const usage = api.usage;
export const pricing = api.pricing;
export const oauth = api.oauth;
