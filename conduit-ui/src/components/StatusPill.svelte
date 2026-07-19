<script lang="ts">
  import { statusClassOf } from "../lib/format";
  import { cn } from "$lib/utils";

  interface Props {
    status?: number;
    errorKind?: string;
    /** In-flight label override. */
    pendingLabel?: string;
  }
  let { status, errorKind, pendingLabel = "…" }: Props = $props();

  const cls = $derived(statusClassOf(status, errorKind));
  const label = $derived(
    errorKind != null ? errorKind : status != null ? String(status) : pendingLabel,
  );

  const tone: Record<string, string> = {
    ok: "bg-[var(--success-soft)] text-[var(--success)]",
    err: "bg-[var(--danger-soft)] text-[var(--danger)]",
    warn: "bg-[var(--warning-soft)] text-[var(--warning)]",
    pending: "bg-[var(--info-soft)] text-[var(--info)]",
  };
</script>

<span
  class={cn(
    "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 font-mono text-[11px] font-medium whitespace-nowrap",
    tone[cls] ?? tone.pending,
  )}
>
  <span class="h-1.5 w-1.5 rounded-full bg-current" aria-hidden="true"></span>
  {label}
</span>
