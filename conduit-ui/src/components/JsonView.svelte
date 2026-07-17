<script lang="ts">
  interface Props {
    data: unknown;
    /** Truncation threshold in chars (design doc: 256KB). */
    maxChars?: number;
  }
  let { data, maxChars = 256 * 1024 }: Props = $props();

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

<div>
  <pre class="json-view">{rendered.text}</pre>
  {#if rendered.truncated}
    <div class="json-truncated">
      … truncated at {maxChars.toLocaleString()} / {rendered.total.toLocaleString()} chars —
      inspect with <span class="mono">conduitctl trace get</span>
    </div>
  {/if}
</div>
