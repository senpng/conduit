<script lang="ts">
  import { app } from "../state/app.svelte";
  import { cn } from "$lib/utils";
  import { X } from "@lucide/svelte";

  const tone: Record<string, string> = {
    ok: "border-[color-mix(in_srgb,var(--success)_35%,var(--border))]",
    error: "border-[color-mix(in_srgb,var(--danger)_35%,var(--border))]",
    warn: "border-[color-mix(in_srgb,var(--warning)_35%,var(--border))]",
    info: "border-[var(--border)]",
  };
</script>

<div class="pointer-events-none fixed right-4 bottom-4 z-[60] flex max-w-[min(360px,92vw)] flex-col gap-2">
  {#each app.toasts as t (t.id)}
    <div
      class={cn(
        "pointer-events-auto flex items-start gap-2.5 rounded-lg border bg-[var(--surface)] px-3 py-2.5 text-[13px] text-[var(--text)] shadow-[0_8px_30px_rgb(17_24_39/0.08)]",
        tone[t.kind] ?? tone.info,
      )}
      role="status"
    >
      <span class="min-w-0 flex-1">{t.text}</span>
      <button
        type="button"
        class="shrink-0 cursor-pointer border-0 bg-transparent p-0.5 text-[var(--text-muted)] hover:text-[var(--text)]"
        aria-label="dismiss"
        onclick={() => app.dismissToast(t.id)}
      >
        <X class="h-3.5 w-3.5" />
      </button>
    </div>
  {/each}
</div>
