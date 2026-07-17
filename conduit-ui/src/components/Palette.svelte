<script lang="ts">
  import { app, SCREENS, type ScreenId } from "../state/app.svelte";
  import { live } from "../state/live.svelte";
  import { fuzzyFilter } from "../lib/fuzzy";

  interface PaletteCommand {
    id: string;
    title: string;
    hint?: string;
    run: () => void;
  }

  let query = $state("");
  let selected = $state(0);
  let inputEl: HTMLInputElement | undefined = $state();

  const commands: PaletteCommand[] = [
    ...SCREENS.map((s) => ({
      id: `goto-${s.id}`,
      title: `Go to ${s.label}`,
      hint: `g ${s.g}`,
      run: () => app.goto(s.id as ScreenId),
    })),
    {
      id: "refresh",
      title: "Refresh current screen",
      hint: "r",
      run: () => app.refreshCurrent(),
    },
    {
      id: "live-pause",
      title: "Live: pause / resume stream",
      run: () => {
        app.goto("live");
        live.togglePause();
      },
    },
    {
      id: "live-clear",
      title: "Live: clear rollup",
      run: () => {
        app.goto("live");
        live.clear();
      },
    },
    {
      id: "help",
      title: "Keyboard shortcuts",
      hint: "?",
      run: () => {
        app.helpOpen = true;
      },
    },
  ];

  const results = $derived(fuzzyFilter(query, commands, (c) => c.title, 12));

  $effect(() => {
    if (app.paletteOpen) {
      query = "";
      selected = 0;
      queueMicrotask(() => inputEl?.focus());
    }
  });

  $effect(() => {
    // Clamp selection when result set shrinks.
    if (selected >= results.length) selected = Math.max(0, results.length - 1);
  });

  function close() {
    app.paletteOpen = false;
  }

  function runAt(i: number) {
    const cmd = results[i];
    if (!cmd) return;
    close();
    cmd.item.run();
  }

  function onKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Escape") {
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      selected = (selected + 1) % Math.max(1, results.length);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selected = (selected - 1 + results.length) % Math.max(1, results.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      runAt(selected);
    }
  }
</script>

{#if app.paletteOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div
    class="palette-backdrop"
    role="presentation"
    onclick={(e) => {
      if (e.target === e.currentTarget) close();
    }}
  >
    <div class="palette" role="dialog" aria-label="Command palette">
      <input
        class="palette-input"
        placeholder="Type a command…"
        bind:value={query}
        bind:this={inputEl}
        onkeydown={onKeydown}
      />
      <div class="palette-list">
        {#each results as r, i (r.item.id)}
          <button
            class="palette-item"
            class:selected={i === selected}
            onmouseenter={() => (selected = i)}
            onclick={() => runAt(i)}
          >
            <span>{r.item.title}</span>
            {#if r.item.hint}
              <span class="kbd">{r.item.hint}</span>
            {/if}
          </button>
        {:else}
          <div class="palette-empty">No matching command</div>
        {/each}
      </div>
    </div>
  </div>
{/if}
