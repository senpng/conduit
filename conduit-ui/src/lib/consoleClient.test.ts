import { describe, it, expect, vi, beforeEach } from "vitest";
import {
  consoleUrl,
  createConsoleApi,
  setConsoleBase,
  getConsoleBase,
  ConsoleClientError,
  buildConsoleHeaders,
} from "./consoleClient";
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

describe("consoleUrl / base", () => {
  beforeEach(() => {
    setConsoleBase("http://127.0.0.1:4001");
  });

  it("builds paths against configured base", () => {
    expect(consoleUrl("/health")).toBe("http://127.0.0.1:4001/health");
    expect(consoleUrl("console/providers")).toBe(
      "http://127.0.0.1:4001/console/providers",
    );
  });

  it("strips trailing slash on base", () => {
    setConsoleBase("http://127.0.0.1:4001/");
    expect(getConsoleBase()).toBe("http://127.0.0.1:4001");
  });
});

describe("providers client", () => {
  it("list uses GET /console/providers", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/console/providers");
      expect(init?.method ?? "GET").toMatch(/GET|undefined/i);
      return jsonResponse([{ id: "p1", name: "OpenAI", kind: "openai" }]);
    });
    const api = createConsoleApi(fetchImpl);
    const rows = await api.providers.list();
    expect(rows[0].id).toBe("p1");
    expect(fetchImpl).toHaveBeenCalledOnce();
  });

  it("create posts name/kind/base_url and optional api_key", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/console/providers");
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
    const api = createConsoleApi(fetchImpl);
    await api.providers.create({
      name: "Mine",
      kind: "openai",
      base_url: "https://api.openai.com",
      api_key: "sk-test",
    });
  });

  it("setSecret uses daemon field api_key not secret", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/console/providers/p1/secret");
      expect(init?.method).toBe("PUT");
      const body = JSON.parse(String(init?.body));
      expect(body).toEqual({ api_key: "sk-live" });
      expect(body.secret).toBeUndefined();
      return emptyResponse();
    });
    const api = createConsoleApi(fetchImpl);
    await api.providers.setSecret("p1", "sk-live");
  });

  it("delete uses DELETE and tolerates 204", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/console/providers/p1");
      expect(init?.method).toBe("DELETE");
      return emptyResponse();
    });
    const api = createConsoleApi(fetchImpl);
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
      expect(url).toBe("http://127.0.0.1:4001/console/routes");
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
    const api = createConsoleApi(fetchImpl);
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
      if (url.endsWith("/console/routes")) return jsonResponse([]);
      return emptyResponse();
    });
    const api = createConsoleApi(fetchImpl);
    await api.routes.list();
    await api.routes.delete("r9");
    expect(calls[0]).toBe("GET http://127.0.0.1:4001/console/routes");
    expect(calls[1]).toBe("DELETE http://127.0.0.1:4001/console/routes/r9");
  });
});

describe("keys client", () => {
  it("create maps token field as key (not raw_key)", async () => {
    const fetchImpl = mockFetch((url, init) => {
      expect(url).toBe("http://127.0.0.1:4001/console/keys");
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
    const api = createConsoleApi(fetchImpl);
    const res = await api.keys.create({ name: "app", rate_limit_rpm: 60 });
    expect(res.key).toBe("sk_abc");
    expect((res as { raw_key?: string }).raw_key).toBeUndefined();
  });
});

describe("usage client", () => {
  it("summary hits /console/usage/summary", async () => {
    const fetchImpl = mockFetch((url) => {
      expect(url).toBe("http://127.0.0.1:4001/console/usage/summary");
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
        by_day: [
          { day: "2026-07-01", request_count: 1, total_usd: 0.5, total_tokens: 5 },
          { day: "2026-07-15", request_count: 1, total_usd: 1.0, total_tokens: 10 },
        ],
        by_model: [
          {
            label: "gpt-5.6-terra",
            provider_kind: "codex-oauth",
            request_count: 2,
            total_usd: 1.5,
            total_tokens: 15,
          },
        ],
      });
    });
    const api = createConsoleApi(fetchImpl);
    const list = await api.usage.summary();
    expect(list.period).toBe("2026-07");
    expect(list.entries).toHaveLength(1);
    expect(list.total_usd).toBe(1.5);
    expect(list.by_day).toHaveLength(2);
    expect(list.by_model?.[0].label).toBe("gpt-5.6-terra");
  });

  it("summary passes period and key_id query params", async () => {
    const fetchImpl = mockFetch((url) => {
      expect(url).toBe(
        "http://127.0.0.1:4001/console/usage/summary?period=2026-06&key_id=k1",
      );
      return jsonResponse({
        period: "2026-06",
        total_usd: 0,
        request_count: 0,
        key_id: "k1",
        entries: [],
        by_day: [],
        by_model: [],
      });
    });
    const api = createConsoleApi(fetchImpl);
    const list = await api.usage.summary("2026-06", "k1");
    expect(list.period).toBe("2026-06");
    expect(list.key_id).toBe("k1");
  });
});


describe("error handling", () => {
  it("throws ConsoleClientError with status and path", async () => {
    const fetchImpl = mockFetch(() =>
      new Response('{"error":"nope"}', { status: 500 }),
    );
    const api = createConsoleApi(fetchImpl);
    await expect(api.health.check()).rejects.toBeInstanceOf(ConsoleClientError);
  });
});

describe("buildConsoleHeaders / preflight avoidance", () => {
  it("does not set Content-Type without a body (GET/DELETE)", () => {
    const h = buildConsoleHeaders({ method: "GET" });
    expect(h.get("Content-Type")).toBeNull();
  });

  it("sets Content-Type only when body is present", () => {
    const h = buildConsoleHeaders({
      method: "POST",
      body: JSON.stringify({ name: "x" }),
    });
    expect(h.get("Content-Type")).toBe("application/json");
  });

  it("preserves Headers and tuple-array inputs", () => {
    const fromHeaders = buildConsoleHeaders({
      headers: new Headers({ Authorization: "Bearer test" }),
    });
    const fromTuples = buildConsoleHeaders({
      headers: [["X-Request-Id", "req-1"]],
    });
    expect(fromHeaders.get("authorization")).toBe("Bearer test");
    expect(fromTuples.get("x-request-id")).toBe("req-1");
  });

  it("preserves an existing lower-case content type", () => {
    const h = buildConsoleHeaders({
      method: "POST",
      body: "payload",
      headers: { "content-type": "application/custom" },
    });
    expect(h.get("content-type")).toBe("application/custom");
  });

  it("GET list does not send Content-Type header via fetch", async () => {
    const fetchImpl = mockFetch((_url, init) => {
      const hdrs = new Headers(init?.headers);
      expect(hdrs.get("content-type")).toBeNull();
      return jsonResponse([]);
    });
    const api = createConsoleApi(fetchImpl);
    await api.providers.list();
  });

  it("POST create does send Content-Type application/json", async () => {
    const fetchImpl = mockFetch((_url, init) => {
      const hdrs = new Headers(init?.headers);
      expect(hdrs.get("content-type")).toBe("application/json");
      return jsonResponse({ id: "p1" }, 201);
    });
    const api = createConsoleApi(fetchImpl);
    await api.providers.create({
      name: "n",
      kind: "openai",
      base_url: "https://api.openai.com",
    });
  });
});

describe("Tauri CSP aligns with default Console base", () => {
  it("connect-src allows 127.0.0.1:4001 and localhost:4001", () => {
    const confPath = resolve(__dirname, "../../src-tauri/tauri.conf.json");
    const conf = JSON.parse(readFileSync(confPath, "utf8"));
    const csp: string = conf.app.security.csp;
    expect(csp).toContain("connect-src");
    expect(csp).toContain("http://127.0.0.1:4001");
    expect(csp).toContain("http://localhost:4001");
    // Default client base must be one of the allowed hosts.
    expect(getConsoleBase()).toBe("http://127.0.0.1:4001");
    expect(csp.includes(getConsoleBase().replace("http://", "http://"))).toBe(
      true,
    );
  });
});
