<script lang="ts">
  import GameListItem from './GameListItem.svelte';
  import {
    getGames,
    getFilteredGames,
    getSearchQuery,
    getSelectedGameId,
    getIsLoading,
    getError,
    scanGames,
    setSearchQuery,
  } from '../stores/games.svelte';

  let filteredGames = $derived(getFilteredGames());
  let totalGames = $derived(getGames().length);
  let selectedId = $derived(getSelectedGameId());
  let currentSearch = $derived(getSearchQuery());
  let isLoading = $derived(getIsLoading());
  let errorMsg = $derived(getError());

  let searchFocused = $state(false);

  function onSearch(e: Event): void {
    setSearchQuery((e.target as HTMLInputElement).value);
  }
</script>

<aside
  style="background: rgba(11, 11, 14, 0.55);"
  class="w-[290px] shrink-0 flex flex-col overflow-hidden border-r border-line"
>
  <!-- header -->
  <div class="px-[15px] pt-[18px] pb-3 shrink-0">
    <div class="flex items-center justify-between mb-[13px]">
      <span class="font-display text-[10.5px] font-semibold tracking-[0.2em] text-t-lo"
        >LINKED GAMES</span
      >
      <span
        style="background: rgba(255,255,255,0.045);"
        class="font-mono text-[10px] text-t-mid px-2 py-0.5 rounded-[7px]"
      >
        {totalGames}
      </span>
    </div>
    <div
      style="
        background: {searchFocused ? 'rgba(255,255,255,0.055)' : 'rgba(255,255,255,0.035)'};
        border-color: {searchFocused
        ? 'color-mix(in oklab, var(--accent) 30%, transparent)'
        : 'var(--color-line)'};
      "
      class="flex items-center gap-2 px-3 py-2 rounded-[10px] border transition-all duration-200"
    >
      <svg
        class="text-t-lo shrink-0"
        fill="none"
        height="14"
        stroke="currentColor"
        stroke-linecap="round"
        stroke-width="2.4"
        viewBox="0 0 24 24"
        width="14"
      >
        <circle cx="11" cy="11" r="8" /><line x1="21" x2="16.65" y1="21" y2="16.65" />
      </svg>
      <input
        class="bg-transparent border-none outline-none text-t-hi font-body text-[12.5px] w-full placeholder:text-t-lo"
        aria-label="Search games"
        onblur={() => (searchFocused = false)}
        onfocus={() => (searchFocused = true)}
        oninput={onSearch}
        placeholder="Search library…"
        type="text"
        value={currentSearch}
      />
    </div>
  </div>

  <!-- list -->
  <div class="flex-1 overflow-y-auto px-[9px] pb-2 flex flex-col gap-[3px]">
    {#if isLoading}
      <div class="flex items-center justify-center h-full text-t-lo text-sm font-display">
        Scanning…
      </div>
    {:else if errorMsg}
      <div class="flex items-center justify-center h-full text-center px-4">
        <div>
          <div class="text-err text-sm font-display mb-2">Scan failed</div>
          <div class="text-t-lo text-xs font-mono">{errorMsg}</div>
        </div>
      </div>
    {:else if filteredGames.length > 0}
      {#each filteredGames as game, i (game.id)}
        <GameListItem {game} index={i} selected={game.id === selectedId} />
      {/each}
    {:else}
      <div class="flex items-center justify-center h-full text-center px-4">
        {#if currentSearch}
          <div class="text-t-lo text-sm font-display">No games match “{currentSearch}”.</div>
        {:else}
          <div>
            <div class="text-t-lo text-sm font-display mb-3">No games bound yet.</div>
            <button
              style="border: 1px solid var(--color-line); color: var(--accent);"
              class="py-2 px-4 rounded-[10px] bg-transparent text-[0.8rem] font-display font-semibold tracking-wide uppercase cursor-pointer transition-all duration-150"
              onclick={() => scanGames()}
              type="button"
            >
              Scan for games
            </button>
          </div>
        {/if}
      </div>
    {/if}
  </div>

  <!-- bind -->
  <button
    style="border: 1px dashed var(--color-line); color: var(--color-t-lo);"
    class="m-3 py-[11px] rounded-[11px] bg-transparent font-display text-[11.5px] font-medium tracking-[0.06em] flex items-center justify-center gap-2 cursor-pointer transition-all duration-150 shrink-0"
    disabled={isLoading}
    onclick={() => scanGames()}
    onmouseenter={(e) => {
      const el = e.currentTarget as HTMLElement;
      el.style.color = 'var(--accent)';
      el.style.borderColor = 'color-mix(in oklab, var(--accent) 45%, transparent)';
      el.style.background = 'color-mix(in oklab, var(--accent) 6%, transparent)';
    }}
    onmouseleave={(e) => {
      const el = e.currentTarget as HTMLElement;
      el.style.color = 'var(--color-t-lo)';
      el.style.borderColor = 'var(--color-line)';
      el.style.background = 'transparent';
    }}
    type="button"
  >
    <svg
      fill="none"
      height="13"
      stroke="currentColor"
      stroke-linecap="round"
      stroke-width="2.4"
      viewBox="0 0 24 24"
      width="13"
    >
      <line x1="12" x2="12" y1="5" y2="19" /><line x1="5" x2="19" y1="12" y2="12" />
    </svg>
    {isLoading ? 'Scanning…' : 'Bind a new game'}
  </button>
</aside>
