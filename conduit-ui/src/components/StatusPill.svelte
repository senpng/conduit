<script lang="ts">
  import { statusClassOf } from "../lib/format";

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
</script>

<span class="pill {cls}">{label}</span>
