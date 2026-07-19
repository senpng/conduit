<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";
  import { cn } from "$lib/utils";

  interface Props {
    label: string;
    value: string | number;
    sub?: string;
    valueClass?: string;
    class?: string;
    onclick?: HTMLButtonAttributes["onclick"];
    children?: Snippet;
  }

  let {
    label,
    value,
    sub,
    valueClass = "",
    class: className = "",
    onclick,
    children,
  }: Props = $props();

  const shell =
    "flex flex-col gap-1.5 rounded-[14px] border border-[var(--border)] bg-[var(--surface)] p-4 text-left shadow-[var(--shadow-sm)]";
</script>

{#if onclick}
  <button
    type="button"
    class={cn(
      shell,
      "cursor-pointer transition-all hover:-translate-y-0.5 hover:border-[var(--border-strong)] hover:shadow-[var(--shadow)]",
      className,
    )}
    {onclick}
  >
    <span class="text-[11px] font-semibold tracking-wide text-[var(--text-secondary)] uppercase">
      {label}
    </span>
    <span class={cn("text-2xl font-semibold tracking-tight text-[var(--text)]", valueClass)}>
      {value}
    </span>
    {#if sub}
      <span class="text-xs text-[var(--text-muted)]">{sub}</span>
    {/if}
    {@render children?.()}
  </button>
{:else}
  <div class={cn(shell, className)}>
    <span class="text-[11px] font-semibold tracking-wide text-[var(--text-secondary)] uppercase">
      {label}
    </span>
    <span class={cn("text-2xl font-semibold tracking-tight text-[var(--text)]", valueClass)}>
      {value}
    </span>
    {#if sub}
      <span class="text-xs text-[var(--text-muted)]">{sub}</span>
    {/if}
    {@render children?.()}
  </div>
{/if}
