/**
 * Pure HTTP admin client for conduitd loopback admin API.
 *
 * Injectable `fetch` for unit tests. Default base: http://127.0.0.1:4001
 * (override via VITE_CONDUIT_ADMIN_URL or setAdminBase).
 */

export type FetchLike = typeof fetch;

const DEFAULT_ADMIN_BASE = "http://127.0.0.1:4001";

let adminBase =
  (typeof import.meta !== "undefined" &&
    (import.meta as ImportMeta & { env?: { VITE_CONDUIT_ADMIN_URL?: string } }).env
      ?.VITE_CONDUIT_ADMIN_URL) ||
  DEFAULT_ADMIN_BASE;

export function getAdminBase(): string {
  return adminBase.replace(/\/$/, "");
}

export function setAdminBase(url: string): void {
  adminBase = url.replace(/\/$/, "");
}

export function adminUrl(path: string, base = getAdminBase()): string {
  const p = path.startsWith("/") ? path : `/${path}`;
  return `${base}${p}`;
}

export class AdminClientError extends Error {
  constructor(
    public readonly status: number,
    public readonly path: string,
    public readonly body: string,
  ) {
    super(`${status} ${path}: ${body}`);
    this.name = "AdminClientError";
  }
}

export type RequestOptions = RequestInit & {
  /** Override fetch implementation (tests). */
  fetchImpl?: FetchLike;
  /** When true, do not parse JSON body (204 etc.). */
  empty?: boolean;
};

/**
 * Build request headers for the admin client.
 * Only sets `Content-Type: application/json` when a body is present so simple
 * GET/DELETE avoid unnecessary CORS preflight from the Tauri/dev origin.
 */
export function buildAdminHeaders(
  init: RequestInit = {},
): Record<string, string> {
  const incoming = (init.headers ?? {}) as Record<string, string>;
  const headers: Record<string, string> = { ...incoming };
  const hasBody = init.body != null && init.body !== "";
  const hasContentType = Object.keys(headers).some(
    (k) => k.toLowerCase() === "content-type",
  );
  if (hasBody && !hasContentType) {
    headers["Content-Type"] = "application/json";
  }
  return headers;
}

export async function adminRequest<T>(
  path: string,
  init: RequestOptions = {},
): Promise<T> {
  const { fetchImpl = fetch, empty = false, headers, ...rest } = init;
  const url = adminUrl(path);
  const mergedHeaders = buildAdminHeaders({
    ...rest,
    headers: headers as Record<string, string> | undefined,
  });
  const res = await fetchImpl(url, {
    ...rest,
    headers: mergedHeaders,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new AdminClientError(res.status, path, text);
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
  /** Present when daemon supports the trace switch. */
  trace_enabled?: boolean;
}

export interface SettingsResponse {
  trace: {
    enabled: boolean;
    config_default?: boolean;
    runtime_override?: boolean | null;
    max_segment_mb?: number;
    max_db_size_mb?: number;
    retention_days?: number;
  };
}

export interface TraceIndexRow {
  id: string;
  /** Shared request audit id (all events of one call). */
  trace_id?: string;
  kind?: string;
  ts?: string;
  alias?: string;
  provider_id?: string | null;
  model_id?: string | null;
  status_code?: number;
  latency_ms?: number;
  cost_usd?: number;
  error_kind?: string | null;
}

/** Complete audit bundle from GET /admin/traces/{id}. */
export interface TraceAuditBundle {
  trace_id: string;
  events: unknown[];
  request?: unknown;
  request_ir?: unknown;
  request_headers?: Record<string, string | string[]> | null;
  response?: unknown;
  response_headers?: Record<string, string | string[]> | null;
  wire_format?: string | null;
  stream?: boolean;
  stream_frames?: string[] | null;
}

export interface TraceListResponse {
  traces: TraceIndexRow[];
}

export interface ReplayPlan {
  dry_run: boolean;
  trace_id: string;
  event_kind?: string;
  request_summary?: unknown;
  intended_target?: unknown;
  routing_error?: string | null;
  upstream_called?: boolean;
  billed?: boolean;
  note?: string;
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

export function createAdminApi(fetchImpl?: FetchLike) {
  const opts = (init?: RequestOptions): RequestOptions => ({
    ...init,
    fetchImpl: init?.fetchImpl ?? fetchImpl,
  });

  return {
    health: {
      check: () => adminRequest<HealthResponse>("/health", opts()),
    },

    settings: {
      get: () => adminRequest<SettingsResponse>("/admin/settings", opts()),
      update: (body: { trace?: { enabled?: boolean } }) =>
        adminRequest<SettingsResponse>("/admin/settings", {
          ...opts(),
          method: "PUT",
          body: JSON.stringify(body),
        }),
    },

    providers: {
      list: () => adminRequest<Provider[]>("/admin/providers", opts()),
      get: (id: string) =>
        adminRequest<Provider>(`/admin/providers/${id}`, opts()),
      create: (body: CreateProviderBody) =>
        adminRequest<Provider>("/admin/providers", {
          ...opts(),
          method: "POST",
          body: JSON.stringify(body),
        }),
      update: (
        id: string,
        body: { name?: string; base_url?: string },
      ) =>
        adminRequest<Provider>(`/admin/providers/${id}`, {
          ...opts(),
          method: "PUT",
          body: JSON.stringify(body),
        }),
      delete: (id: string) =>
        adminRequest<void>(`/admin/providers/${id}`, {
          ...opts({ empty: true }),
          method: "DELETE",
        }),
      /** Daemon expects `{ api_key: string }`. */
      setSecret: (id: string, api_key: string) =>
        adminRequest<void>(`/admin/providers/${id}/secret`, {
          ...opts({ empty: true }),
          method: "PUT",
          body: JSON.stringify({ api_key }),
        }),
    },

    routes: {
      list: () => adminRequest<Route[]>("/admin/routes", opts()),
      get: (id: string) => adminRequest<Route>(`/admin/routes/${id}`, opts()),
      create: (body: CreateRouteBody) =>
        adminRequest<Route>("/admin/routes", {
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
        adminRequest<Route>(`/admin/routes/${id}`, {
          ...opts(),
          method: "PUT",
          body: JSON.stringify(body),
        }),
      delete: (id: string) =>
        adminRequest<void>(`/admin/routes/${id}`, {
          ...opts({ empty: true }),
          method: "DELETE",
        }),
    },

    keys: {
      list: () => adminRequest<DownstreamKey[]>("/admin/keys", opts()),
      get: (id: string) =>
        adminRequest<DownstreamKey>(`/admin/keys/${id}`, opts()),
      create: (body: {
        name: string;
        model_whitelist?: string[];
        rate_limit_rpm?: number;
      }) =>
        adminRequest<CreateKeyResponse>("/admin/keys", {
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
        adminRequest<DownstreamKey>(`/admin/keys/${id}`, {
          ...opts(),
          method: "PUT",
          body: JSON.stringify(body),
        }),
      delete: (id: string) =>
        adminRequest<void>(`/admin/keys/${id}`, {
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
        return adminRequest<UsageSummaryResponse>(`/admin/usage/summary${q}`, opts());
      },
      list: async (limit = 50, keyId?: string) => {
        let path = `/admin/usage?limit=${encodeURIComponent(String(limit))}`;
        if (keyId) path += `&key_id=${encodeURIComponent(keyId)}`;
        return adminRequest<UsageListResponse>(path, opts());
      },
    },

    pricing: {
      list: () => adminRequest<PricingRow[]>("/admin/pricing", opts()),
      reload: () =>
        adminRequest<{ status: string }>("/admin/pricing/reload", {
          ...opts(),
          method: "POST",
        }),
      /** Fetch LiteLLM cost map, convert, cache as pricing.litellm.json, reload. */
      sync: (url?: string) =>
        adminRequest<{
          status: string;
          source?: string;
          url?: string;
          sync_date?: string;
          source_models?: number;
          skipped?: number;
          total_rows?: number;
        }>("/admin/pricing/sync", {
          ...opts(),
          method: "POST",
          body: url ? JSON.stringify({ url }) : JSON.stringify({}),
        }),
    },

    traces: {
      /** Default list is one row per request; `all=true` lists every event. */
      list: (limit = 20, all = false) =>
        adminRequest<TraceListResponse>(
          `/admin/traces?limit=${encodeURIComponent(String(limit))}${all ? "&all=true" : ""}`,
          opts(),
        ),
      get: (id: string) =>
        adminRequest<unknown>(`/admin/traces/${encodeURIComponent(id)}`, opts()),
      /** Default dry-run; live execute is not supported by daemon. */
      replay: (id: string, dryRun = true) =>
        adminRequest<ReplayPlan>(
          `/admin/traces/${encodeURIComponent(id)}/replay?dry_run=${dryRun}`,
          {
            ...opts(),
            method: "POST",
          },
        ),
      streamUrl: () => adminUrl("/admin/traces/stream"),
    },

    oauth: {
      listProviders: () =>
        adminRequest<OAuthProviderMeta[]>("/admin/oauth/providers", opts()),
      start: (
        kind: string,
        body: { name?: string; provider_id?: string } = {},
      ) =>
        adminRequest<OAuthSession>(`/admin/oauth/${encodeURIComponent(kind)}/start`, {
          ...opts(),
          method: "POST",
          body: JSON.stringify(body),
        }),
      session: (id: string) =>
        adminRequest<OAuthSession>(
          `/admin/oauth/sessions/${encodeURIComponent(id)}`,
          opts(),
        ),
      cancel: (id: string) =>
        adminRequest<{ ok: boolean }>(
          `/admin/oauth/sessions/${encodeURIComponent(id)}/cancel`,
          {
            ...opts(),
            method: "POST",
          },
        ),
      refresh: (providerId: string) =>
        adminRequest<unknown>(
          `/admin/oauth/${encodeURIComponent(providerId)}/refresh`,
          {
            ...opts(),
            method: "POST",
          },
        ),
    },
  };
}

/** Default singleton wired to global fetch. */
export const api = createAdminApi();

// Back-compat named exports used by views
export const health = api.health;
export const settings = api.settings;
export const providers = api.providers;
export const routes = api.routes;
export const keys = api.keys;
export const usage = api.usage;
export const pricing = api.pricing;
export const traces = api.traces;
export const oauth = api.oauth;
