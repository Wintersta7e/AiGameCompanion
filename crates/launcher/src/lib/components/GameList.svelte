<script lang="ts">
  import GameListItem from "./GameListItem.svelte";
  import {
    getFilteredGames,
    getFilterSource,
    getSearchQuery,
    getSelectedGameId,
    getIsLoading,
    getError,
    scanGames,
    setFilterSource,
  } from "../stores/games.svelte";
  import type { FilterSource } from "../stores/games.svelte";

  const filters: readonly { label: string; value: FilterSource }[] = [
    { label: "All", value: "all" },
    { label: "Steam", value: "steam" },
    { label: "Epic", value: "epic" },
    { label: "GOG", value: "gog" },
  ] as const;

  let filteredGames = $derived(getFilteredGames());
  let activeFilter = $derived(getFilterSource());
  let selectedId = $derived(getSelectedGameId());
  let currentSearch = $derived(getSearchQuery());
  let isLoading = $derived(getIsLoading());
  let errorMsg = $derived(getError());

  function handleFilterClick(value: FilterSource): void {
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
          aria-pressed={activeFilter === filter.value}
          onclick={() => handleFilterClick(filter.value)}
        >
          {filter.label}
        </button>
      {/each}
    </div>
  </div>

  <!-- Game list -->
  <div class="flex-1 overflow-y-auto p-2">
    {#if isLoading}
      <div class="flex items-center justify-center h-full">
        <div class="text-text-muted text-sm font-display">Scanning games...</div>
      </div>
    {:else if errorMsg}
      <div class="flex items-center justify-center h-full">
        <div class="text-center px-4">
          <div class="text-accent-warm text-sm font-display mb-2">Failed to scan games</div>
          <div class="text-text-muted text-xs font-mono">{errorMsg}</div>
        </div>
      </div>
    {:else if filteredGames.length > 0}
      {#each filteredGames as game, i (game.id)}
        <GameListItem {game} selected={game.id === selectedId} index={i} />
      {/each}
    {:else}
      <div class="flex items-center justify-center h-full">
        <div class="text-center px-4">
          {#if currentSearch}
            <div class="text-text-muted text-sm font-display">No games match your search</div>
          {:else if activeFilter !== "all"}
            <div class="text-text-muted text-sm font-display">No {activeFilter} games found</div>
          {:else}
            <div class="text-text-muted text-sm font-display mb-3">No games found</div>
            <button
              class="py-2 px-4 border border-border-subtle rounded-[10px] bg-transparent text-accent font-display text-[0.8rem] font-semibold tracking-wide uppercase cursor-pointer transition-all duration-150 hover:border-border-glow hover:bg-[rgba(99,140,255,0.08)]"
              onclick={() => scanGames()}
            >
              Scan for Games
            </button>
          {/if}
        </div>
      </div>
    {/if}
  </div>

  <!-- Rescan button -->
  <button
    class="m-2 py-2.5 px-3 border border-dashed border-border-subtle rounded-[10px] bg-transparent text-text-muted font-display text-[0.8rem] font-semibold tracking-wide uppercase transition-all duration-150 text-center cursor-pointer hover:text-accent hover:border-border-glow hover:bg-[rgba(99,140,255,0.05)]"
    onclick={() => scanGames()}
    disabled={isLoading}
    class:opacity-50={isLoading}
    class:cursor-not-allowed={isLoading}
  >
    {isLoading ? "Scanning..." : "Rescan Games"}
  </button>
</aside>
