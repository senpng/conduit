<script lang="ts">
  import type { Snippet } from "svelte";
  import { cn } from "$lib/utils";
  import { AlertTriangle, Info, XCircle } from "@lucide/svelte";

  interface Props {
    variant?: "info" | "warning" | "danger";
    class?: string;
    children?: Snippet;
  }

  let { variant = "info", class: className = "", children }: Props = $props();

  const styles = {
    info: "border-[color-mix(in_srgb,var(--info)_30%,var(--border))] bg-[var(--info-soft)] text-[var(--info)]",
    warning:
      "border-[color-mix(in_srgb,var(--warning)_30%,var(--border))] bg-[var(--warning-soft)] text-[var(--warning)]",
    danger:
      "border-[color-mix(in_srgb,var(--danger)_30%,var(--border))] bg-[var(--danger-soft)] text-[var(--danger)]",
  } as const;
</script>

<div
  class={cn(
    "flex gap-2.5 rounded-lg border px-3 py-2.5 text-[13px] leading-relaxed",
    styles[variant],
    className,
  )}
  role="status"
>
  {#if variant === "warning"}
    <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0" />
  {:else if variant === "danger"}
    <XCircle class="mt-0.5 h-4 w-4 shrink-0" />
  {:else}
    <Info class="mt-0.5 h-4 w-4 shrink-0" />
  {/if}
  <div class="min-w-0 flex-1">{@render children?.()}</div>
</div>
