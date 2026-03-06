<script lang="ts">
  import GameListItem from "./GameListItem.svelte";
  import {
    getFilteredGames,
    getFilterSource,
    getSelectedGameId,
    setFilterSource,
  } from "../stores/games.svelte";

  const filters = [
    { label: "All", value: "all" },
    { label: "Steam", value: "steam" },
    { label: "Epic", value: "epic" },
    { label: "GOG", value: "gog" },
  ] as const;

  let filteredGames = $derived(getFilteredGames());
  let activeFilter = $derived(getFilterSource());
  let selectedId = $derived(getSelectedGameId());

  function handleFilterClick(value: string): void {
    setFilterSource(value);
  }
</script>

<aside
  class="w-[300px] shrink-0 flex flex-col overflow-hidden border-r border-border-subtle"
  style="background: rgba(17, 21, 34, 0.7); backdrop-filter: blur(10px);"
>
  <!-- Filter tabs -->
  <div class="p-4 shrink-0 border-b border-border-subtle">
    <div class="flex gap-0.5 rounded-md p-0.5" style="background: rgba(255, 255, 255, 0.03);">
      {#each filters as filter}
        <button
          class="flex-1 py-1.5 px-1 font-display text-[0.72rem] font-semibold tracking-wide uppercase text-center border-none rounded cursor-pointer transition-all duration-150"
          class:text-accent={activeFilter === filter.value}
          class:bg-[rgba(99,140,255,0.12)]={activeFilter === filter.value}
          class:text-text-muted={activeFilter !== filter.value}
          class:bg-transparent={activeFilter !== filter.value}
          class:hover:text-text-secondary={activeFilter !== filter.value}
          onclick={() => handleFilterClick(filter.value)}
        >
          {filter.label}
        </button>
      {/each}
    </div>
  </div>

  <!-- Game list -->
  <div class="flex-1 overflow-y-auto p-2">
    {#each filteredGames as game, i (game.id)}
      <GameListItem {game} selected={game.id === selectedId} index={i} />
    {/each}
  </div>

  <!-- Add game button -->
  <button
    class="m-2 py-2.5 px-3 border border-dashed border-border-subtle rounded-[10px] bg-transparent text-text-muted font-display text-[0.8rem] font-semibold tracking-wide uppercase cursor-pointer transition-all duration-150 text-center hover:border-border-glow hover:text-accent hover:bg-[rgba(99,140,255,0.04)]"
  >
    + Add Game
  </button>
</aside>
