<script lang="ts">
  import { convertFileSrc } from "@tauri-apps/api/core";
  import type { Game } from "../stores/games.svelte";
  import { setSelectedGameId, getGameStatus } from "../stores/games.svelte";
  import { formatPlayTime } from "../utils/format";

  interface Props {
    game: Game;
    selected: boolean;
    index: number;
  }

  let { game, selected, index }: Props = $props();

  const sourceColors: Record<string, string> = {
    steam: "#1b9aff",
    epic: "#ccc",
    gog: "#b035e8",
    manual: "#06d6a0",
  };

  let coverSrc = $derived(
    game.cover_art_path ? convertFileSrc(game.cover_art_path) : null,
  );

  let playTimeFormatted = $derived(formatPlayTime(game.play_time_minutes));

  let status = $derived(getGameStatus(game.id));

  let statusDotColor = $derived(
    status === "launching"
      ? "#ffc107"
      : status === "injecting"
        ? "#638cff"
        : status === "error"
          ? "#ff6b6b"
          : "#06d6a0",
  );

  let statusDotShadow = $derived(
    status === "launching"
      ? "0 0 6px rgba(255, 193, 7, 0.4)"
      : status === "injecting"
        ? "0 0 6px rgba(99, 140, 255, 0.4)"
        : status === "error"
          ? "0 0 6px rgba(255, 107, 107, 0.4)"
          : "0 0 6px rgba(6, 214, 160, 0.4)",
  );

  function handleClick(): void {
    setSelectedGameId(game.id);
  }
</script>

<button
  class="w-full flex items-center gap-3 px-3 py-2.5 rounded-[10px] cursor-pointer border mb-0.5 text-left transition-all duration-150"
  class:bg-[rgba(99,140,255,0.08)]={selected}
  class:border-border-glow={selected}
  class:border-transparent={!selected}
  class:hover:bg-[rgba(99,140,255,0.05)]={!selected}
  class:hover:border-border-subtle={!selected}
  onclick={handleClick}
  style="animation: card-in 0.3s ease-out {index * 0.03}s both;"
>
  <!-- Cover art thumbnail -->
  <div
    class="w-[42px] h-[56px] rounded-md overflow-hidden shrink-0"
    style="box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);"
  >
    {#if coverSrc}
      <img
        src={coverSrc}
        alt={game.name}
        class="w-full h-full object-cover"
        loading="lazy"
      />
    {:else}
      <div
        class="w-full h-full"
        style="background: linear-gradient(135deg, rgba(99, 140, 255, 0.2) 0%, rgba(168, 85, 247, 0.2) 100%);"
      ></div>
    {/if}
  </div>

  <!-- Game info -->
  <div class="flex-1 min-w-0">
    <div class="font-display text-[0.88rem] font-semibold text-text-primary truncate">
      {game.name}
    </div>
    <div class="text-[0.7rem] text-text-muted mt-0.5 flex items-center gap-1.5">
      <span
        class="w-1.5 h-1.5 rounded-full inline-block shrink-0"
        style="background: {sourceColors[game.source] ?? '#06d6a0'};"
      ></span>
      <span class="uppercase tracking-wide">{game.source}</span>
      <span>&#183;</span>
      <span>{playTimeFormatted}</span>
    </div>
  </div>

  <!-- Status dot -->
  <div
    class="w-[7px] h-[7px] rounded-full shrink-0"
    style="background: {statusDotColor}; box-shadow: {statusDotShadow};"
  ></div>
</button>
