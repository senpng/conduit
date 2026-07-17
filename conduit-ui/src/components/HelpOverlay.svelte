<script lang="ts">
  import { app } from "../state/app.svelte";
  import Modal from "./Modal.svelte";
</script>

{#if app.helpOpen}
  <Modal onclose={() => (app.helpOpen = false)}>
    <h3>Keyboard shortcuts</h3>
    <div class="help-grid">
      <div class="help-section">Global</div>
      <span class="kbd">Ctrl+K</span><span class="dim">Command palette</span>
      <span class="kbd">?</span><span class="dim">This help</span>
      <span class="kbd">r</span><span class="dim">Refresh current screen</span>
      <span class="kbd">Esc</span><span class="dim">Close overlay / blur input</span>

      <div class="help-section">Navigation (press g, then key)</div>
      <span class="kbd">g d</span><span class="dim">Dashboard</span>
      <span class="kbd">g m</span><span class="dim">Live Monitor</span>
      <span class="kbd">g t</span><span class="dim">Traces</span>
      <span class="kbd">g p</span><span class="dim">Providers</span>
      <span class="kbd">g r</span><span class="dim">Routes</span>
      <span class="kbd">g k</span><span class="dim">Keys</span>
      <span class="kbd">g b</span><span class="dim">Usage</span>
      <span class="kbd">g i</span><span class="dim">Pricing</span>
      <span class="kbd">g g</span><span class="dim">Scroll to top</span>

      <div class="help-section">Lists &amp; dialogs</div>
      <span class="kbd">Enter</span><span class="dim">Open focused row / trace detail</span>
      <span class="kbd">y</span><span class="dim">Confirm destructive action</span>
      <span class="kbd">n</span><span class="dim">Cancel dialog</span>
    </div>
    <div class="form-actions">
      <button class="btn-primary" onclick={() => (app.helpOpen = false)}>Close</button>
    </div>
  </Modal>
{/if}
