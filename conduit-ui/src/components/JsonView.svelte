<script lang="ts">
  import { cn } from "$lib/utils";

  interface Props {
    data: unknown;
    /** Truncation threshold in chars (design doc: 256KB). */
    maxChars?: number;
    class?: string;
  }
  let { data, maxChars = 256 * 1024, class: className = "" }: Props = $props();

  const rendered = $derived.by(() => {
    let text: string;
    try {
      text = typeof data === "string" ? data : JSON.stringify(data, null, 2);
    } catch {
      text = String(data);
    }
    if (text.length > maxChars) {
      return { text: text.slice(0, maxChars), truncated: true, total: text.length };
    }
    return { text, truncated: false, total: text.length };
  });
</script>

<div class={className}>
  <pre
    class={cn(
      "max-h-[60vh] overflow-auto rounded-lg border border-[var(--border)] bg-[var(--surface-muted)] p-3 font-mono text-xs leading-relaxed text-[var(--text)] whitespace-pre-wrap break-words",
    )}>{rendered.text}</pre>
  {#if rendered.truncated}
    <div class="mt-1 text-[11px] text-[var(--text-muted)]">
      … truncated at {maxChars.toLocaleString()} / {rendered.total.toLocaleString()} chars —
      truncated in the UI or fetch via the console API
    </div>
  {/if}
</div>
