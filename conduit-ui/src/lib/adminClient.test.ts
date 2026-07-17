import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  adminUrl,
  createAdminApi,
  setAdminBase,
  getAdminBase,
  AdminClientError,
  buildAdminHeaders,
} from "./adminClient";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

function mockFetch(
  handler: (url: string, init?: RequestInit) => Promise<Response> | Response,
) {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    return handler(url, init);
  }) as unknown as typeof fetch;
}

function jsonResponse(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function emptyResponse(status = 204): Response {
  return new Response(null, { status });
}

describe("adminUrl / base", () => {
  beforeEach(() => {
    setAdminBase("http://127.0.0.1:4001");
  });

  it("builds paths against configured base", () => {
    expect(adminUrl("/health")).toBe("http://127.0.0.1:4001/health");
    expect(adminUrl("admin/providers")).toBe(
      "http://127.0.0.1:4001/admin/providers",
    );
  });

  it("strips trailing slash on base", () => {
    setAdminBase("http://127.0.0.1:4001/");
    expect(getAdminBase()).toBe("http://127.0.0.1:4001");
  });
});

describe("providers client", () => {
  it("list uses GET /admin/providers", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/admin/providers");
      expect(init?.method ?? "GET").toMatch(/GET|undefined/i);
      return jsonResponse([{ id: "p1", name: "OpenAI", kind: "openai" }]);
    });
    const api = createAdminApi(fetchImpl);
    const rows = await api.providers.list();
    expect(rows[0].id).toBe("p1");
    expect(fetchImpl).toHaveBeenCalledOnce();
  });

  it("create posts name/kind/base_url and optional api_key", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/admin/providers");
      expect(init?.method).toBe("POST");
      const body = JSON.parse(String(init?.body));
      expect(body).toEqual({
        name: "Mine",
        kind: "openai",
        base_url: "https://api.openai.com",
        api_key: "sk-test",
      });
      expect(body.secret).toBeUndefined();
      return jsonResponse({ id: "p2", ...body, upstream_key_ref: "x" }, 201);
    });
    const api = createAdminApi(fetchImpl);
    await api.providers.create({
      name: "Mine",
      kind: "openai",
      base_url: "https://api.openai.com",
      api_key: "sk-test",
    });
  });

  it("setSecret uses daemon field api_key not secret", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/admin/providers/p1/secret");
      expect(init?.method).toBe("PUT");
      const body = JSON.parse(String(init?.body));
      expect(body).toEqual({ api_key: "sk-live" });
      expect(body.secret).toBeUndefined();
      return emptyResponse();
    });
    const api = createAdminApi(fetchImpl);
    await api.providers.setSecret("p1", "sk-live");
  });

  it("delete uses DELETE and tolerates 204", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/admin/providers/p1");
      expect(init?.method).toBe("DELETE");
      return emptyResponse();
    });
    const api = createAdminApi(fetchImpl);
    await api.providers.delete("p1");
  });
});

describe("routes client", () => {
  it("create sends targets as JSON value not targets_json string field", async () => {
    const targets = [
      {
        provider_id: "p1",
        model_id: "gpt-4o",
        upstream_key_id: "k1",
        provider_kind: "openai",
      },
    ];
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/admin/routes");
      expect(init?.method).toBe("POST");
      const body = JSON.parse(String(init?.body));
      expect(body.match_alias).toBe("gpt-4o");
      expect(body.strategy).toBe("fixed");
      expect(body.targets).toEqual(targets);
      expect(body.targets_json).toBeUndefined();
      return jsonResponse(
        {
          id: "r1",
          match_alias: "gpt-4o",
          strategy: "fixed",
          targets_json: JSON.stringify(targets),
          retry_policy_json: "{}",
          enabled: true,
        },
        201,
      );
    });
    const api = createAdminApi(fetchImpl);
    await api.routes.create({
      match_alias: "gpt-4o",
      strategy: "fixed",
      targets,
    });
  });

  it("list and delete hit correct paths", async () => {
    const calls: string[] = [];
    const fetchImpl = mockFetch((url, init) => {
      calls.push(`${init?.method ?? "GET"} ${url}`);
      if (url.endsWith("/admin/routes")) return jsonResponse([]);
      return emptyResponse();
    });
    const api = createAdminApi(fetchImpl);
    await api.routes.list();
    await api.routes.delete("r9");
    expect(calls[0]).toBe("GET http://127.0.0.1:4001/admin/routes");
    expect(calls[1]).toBe("DELETE http://127.0.0.1:4001/admin/routes/r9");
  });
});

describe("keys client", () => {
  it("create maps token field as key (not raw_key)", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/admin/keys");
      expect(init?.method).toBe("POST");
      return jsonResponse(
        {
          id: "k1",
          key: "sk_abc",
          name: "app",
          model_whitelist: [],
          rate_limit_rpm: 60,
          created_at: "2026-01-01T00:00:00Z",
        },
        201,
      );
    });
    const api = createAdminApi(fetchImpl);
    const res = await api.keys.create({ name: "app", rate_limit_rpm: 60 });
    expect(res.key).toBe("sk_abc");
    expect((res as { raw_key?: string }).raw_key).toBeUndefined();
  });
});

describe("usage client", () => {
  it("summary hits /admin/usage/summary", async () => {
    const fetchImpl = mockFetch((url) => {
      expect(url).toBe("http://127.0.0.1:4001/admin/usage/summary");
      return jsonResponse({
        period: "2026-07",
        total_usd: 1.5,
        request_count: 2,
        entries: [
          {
            downstream_key_id: "k1",
            request_count: 2,
            total_usd: 1.5,
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
          },
        ],
      });
    });
    const api = createAdminApi(fetchImpl);
    const list = await api.usage.summary();
    expect(list.period).toBe("2026-07");
    expect(list.entries).toHaveLength(1);
    expect(list.total_usd).toBe(1.5);
  });
});

describe("traces client", () => {
  it("list/get/replay use admin trace paths with dry_run default", async () => {
    const calls: { method: string; url: string; body?: string }[] = [];
    const fetchImpl = mockFetch((url, init) => {
      calls.push({
        method: init?.method ?? "GET",
        url,
        body: init?.body ? String(init.body) : undefined,
      });
      if (url.includes("/replay")) {
        return jsonResponse({
          dry_run: true,
          trace_id: "t1",
          upstream_called: false,
          billed: false,
          intended_target: { provider_kind: "openai", model_id: "gpt-4o" },
        });
      }
      if (url.includes("/admin/traces/t1")) {
        return jsonResponse({ id: "t1", kind: { type: "request_received" } });
      }
      return jsonResponse({ traces: [{ id: "t1", alias: "gpt-4o" }] });
    });
    const api = createAdminApi(fetchImpl);
    const list = await api.traces.list(10);
    expect(list.traces[0].id).toBe("t1");
    await api.traces.get("t1");
    const plan = await api.traces.replay("t1");
    expect(plan.dry_run).toBe(true);
    expect(plan.billed).toBe(false);
    expect(api.traces.streamUrl()).toBe(
      "http://127.0.0.1:4001/admin/traces/stream",
    );

    expect(calls[0].url).toContain("/admin/traces?limit=10");
    expect(calls[1].url).toContain("/admin/traces/t1");
    expect(calls[2].method).toBe("POST");
    expect(calls[2].url).toContain("/admin/traces/t1/replay?dry_run=true");
  });
});

describe("error handling", () => {
  it("throws AdminClientError with status and path", async () => {
    const fetchImpl = mockFetch(() =>
      new Response('{"error":"nope"}', { status: 500 }),
    );
    const api = createAdminApi(fetchImpl);
    await expect(api.health.check()).rejects.toBeInstanceOf(AdminClientError);
  });
});

describe("buildAdminHeaders / preflight avoidance", () => {
  it("does not set Content-Type without a body (GET/DELETE)", () => {
    const h = buildAdminHeaders({ method: "GET" });
    expect(h["Content-Type"]).toBeUndefined();
    expect(h["content-type"]).toBeUndefined();
  });

  it("sets Content-Type only when body is present", () => {
    const h = buildAdminHeaders({
      method: "POST",
      body: JSON.stringify({ name: "x" }),
    });
    expect(h["Content-Type"]).toBe("application/json");
  });

  it("GET list does not send Content-Type header via fetch", async () => {
    const fetchImpl = mockFetch((_url, init) => {
      const hdrs = new Headers(init?.headers);
      expect(hdrs.get("content-type")).toBeNull();
      return jsonResponse([]);
    });
    const api = createAdminApi(fetchImpl);
    await api.providers.list();
  });

  it("POST create does send Content-Type application/json", async () => {
    const fetchImpl = mockFetch((_url, init) => {
      const hdrs = new Headers(init?.headers);
      expect(hdrs.get("content-type")).toBe("application/json");
      return jsonResponse({ id: "p1" }, 201);
    });
    const api = createAdminApi(fetchImpl);
    await api.providers.create({
      name: "n",
      kind: "openai",
      base_url: "https://api.openai.com",
    });
  });
});

describe("Tauri CSP aligns with default admin base", () => {
  it("connect-src allows 127.0.0.1:4001 and localhost:4001", () => {
    const confPath = resolve(__dirname, "../../src-tauri/tauri.conf.json");
    const conf = JSON.parse(readFileSync(confPath, "utf8"));
    const csp: string = conf.app.security.csp;
    expect(csp).toContain("connect-src");
    expect(csp).toContain("http://127.0.0.1:4001");
    expect(csp).toContain("http://localhost:4001");
    // Default client base must be one of the allowed hosts.
    expect(getAdminBase()).toBe("http://127.0.0.1:4001");
    expect(csp.includes(getAdminBase().replace("http://", "http://"))).toBe(
      true,
    );
  });
});
