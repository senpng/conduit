export function fmtUsd(v: number | undefined | null): string {
  if (v == null) return "—";
  return `$${v.toFixed(4)}`;
}

export function fmtUsd2(v: number | undefined | null): string {
  if (v == null) return "—";
  return `$${v.toFixed(2)}`;
}

export function fmtDate(s: string): string {
  try {
    return new Date(s).toLocaleDateString("en", {
      month: "short",
      day: "numeric",
      year: "numeric",
    });
  } catch {
    return s;
  }
}

/** HH:MM:SS local wall time (for live tables). */
export function fmtTime(s: string | undefined | null): string {
  if (!s) return "—";
  try {
    return new Date(s).toLocaleTimeString("en", { hour12: false });
  } catch {
    return s;
  }
}

/** "Jul 15" — day bucket label for charts. */
export function fmtDay(s: string | undefined | null): string {
  if (!s) return "—";
  try {
    return new Date(s).toLocaleDateString("en", { month: "short", day: "numeric" });
  } catch {
    return s;
  }
}

/** Local YYYY-MM-DD bucket key for day aggregation. */
export function dayKey(s: string): string {
  const d = new Date(s);
  if (Number.isNaN(d.getTime())) return s.slice(0, 10);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** Relative age like "3s" / "2m" / "1h" / "5d". */
export function fmtAgo(s: string | undefined | null, now = Date.now()): string {
  if (!s) return "—";
  const t = Date.parse(s);
  if (Number.isNaN(t)) return s;
  const sec = Math.max(0, Math.floor((now - t) / 1000));
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h`;
  return `${Math.floor(hr / 24)}d`;
}

export function fmtMs(v: number | undefined | null): string {
  if (v == null) return "—";
  if (v >= 10_000) return `${(v / 1000).toFixed(1)}s`;
  return `${v}ms`;
}

/** 12_345 → "12.3k" */
export function fmtTokens(v: number | undefined | null): string {
  if (v == null) return "—";
  if (v < 1000) return String(v);
  return `${(v / 1000).toFixed(1)}k`;
}

/** Total token count from a flat usage-ish object. */
export function sumTokens(u: Record<string, number | undefined>): number {
  return (u.prompt_tokens ?? 0) + (u.completion_tokens ?? 0);
}

/** Short id for dense tables: keep head+tail of long ULIDs. */
export function shortId(id: string | undefined | null, head = 6): string {
  if (!id) return "—";
  if (id.length <= head + 3) return id;
  return `${id.slice(0, head)}…`;
}

/** Provider list row shape used for id → name lookup (admin `/admin/providers`). */
export type ProviderNameSource = { id: string; name?: string | null };

/**
 * Prefer provider **name** for UI; fall back to id when name is missing/unknown.
 * Empty / null id → "—".
 */
export function providerDisplayName(
  providers: readonly ProviderNameSource[] | null | undefined,
  providerId: string | null | undefined,
): string {
  if (providerId == null || providerId === "") return "—";
  const hit = providers?.find((p) => p.id === providerId);
  const name = hit?.name?.trim();
  return name || providerId;
}

/** Build a stable id→name map for repeated lookups. */
export function providerNameMap(
  providers: readonly ProviderNameSource[] | null | undefined,
): Map<string, string> {
  const m = new Map<string, string>();
  for (const p of providers ?? []) {
    const name = p.name?.trim();
    if (name) m.set(p.id, name);
  }
  return m;
}

export function statusClassOf(
  status: number | undefined,
  errorKind: string | undefined,
): "ok" | "err" | "warn" | "pending" {
  if (errorKind != null) return "err";
  if (status == null) return "pending";
  if (status >= 200 && status < 300) return "ok";
  if (status >= 400 && status < 500) return "warn";
  return "err";
}
