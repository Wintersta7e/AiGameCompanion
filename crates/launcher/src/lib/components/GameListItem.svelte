<script lang="ts">
  import type { Game } from '../stores/games.svelte';
  import { setSelectedGameId } from '../stores/games.svelte';
  import { formatPlayTime } from '../utils/format';

  interface Props {
    game: Game;
    selected: boolean;
    index: number;
  }

  let { game, selected, index }: Props = $props();

  const sourceColors: Record<string, string> = {
    steam: '#66c0f4',
    epic: '#cfcfcf',
    gog: '#b035e8',
    manual: 'var(--accent)',
  };

  let coverSrc = $derived(game.cover_art_path ?? null);
  let imgError = $state(false);
  $effect(() => {
    if (game) imgError = false;
  });

  let playTimeFormatted = $derived(formatPlayTime(game.play_time_minutes));
  let initial = $derived(
    game.name
      .replace(/[^A-Za-z0-9]/, '')
      .charAt(0)
      .toUpperCase(),
  );
  let dotColor = $derived(sourceColors[game.source] ?? 'var(--accent)');
</script>

<button
  onclick={() => setSelectedGameId(game.id)}
  class="relative flex items-center gap-[11px] w-full text-left px-[11px] py-[9px] rounded-[11px] cursor-pointer transition-all duration-150"
  style="
    animation: card-in 0.3s ease-out {index * 0.03}s both;
    border: 1px solid {selected
    ? 'color-mix(in oklab, var(--accent) 34%, transparent)'
    : 'transparent'};
    background: {selected
    ? 'linear-gradient(90deg, color-mix(in oklab, var(--accent) 16%, transparent), transparent 62%)'
    : 'transparent'};
  "
  onmouseenter={(e) => {
    if (!selected) (e.currentTarget as HTMLElement).style.background = 'rgba(255,255,255,0.035)';
  }}
  onmouseleave={(e) => {
    if (!selected) (e.currentTarget as HTMLElement).style.background = 'transparent';
  }}
>
  <!-- accent rail (selected) -->
  <span
    class="absolute left-0 top-1/2 -translate-y-1/2 w-[3px] rounded-r-[3px] transition-all duration-200"
    style="height: {selected ? '28px' : '0'}; background: var(--accent);"
  ></span>

  <!-- cover thumb -->
  <div
    class="relative w-[38px] h-[52px] rounded-[7px] shrink-0 overflow-hidden"
    style="box-shadow: 0 2px 9px rgba(0,0,0,0.45);"
  >
    {#if coverSrc && !imgError}
      <img
        src={coverSrc}
        alt={game.name}
        class="w-full h-full object-cover"
        loading="lazy"
        onerror={() => (imgError = true)}
      />
    {:else}
      <div
        class="w-full h-full grid place-items-center font-display font-bold text-base text-white/85"
        style="background: linear-gradient(135deg, color-mix(in oklab, var(--accent) 45%, #16161a), #101013); text-shadow: 0 1px 5px rgba(0,0,0,0.55);"
      >
        {initial}
      </div>
    {/if}
    <span class="absolute inset-0" style="box-shadow: inset 0 0 0 1px rgba(255,255,255,0.08);"
    ></span>
  </div>

  <!-- info -->
  <div class="flex-1 min-w-0">
    <div class="font-display text-[13px] font-medium text-t-hi truncate">{game.name}</div>
    <div class="flex items-center gap-[7px] mt-[3px] text-[10.5px] text-t-lo">
      <span
        class="w-[7px] h-[7px] rounded-full shrink-0"
        style="background: {dotColor}; box-shadow: 0 0 6px color-mix(in oklab, {dotColor} 60%, transparent);"
      ></span>
      <span class="uppercase tracking-wide">{game.source}</span>
      <span class="opacity-50">·</span>
      <span class="font-mono">{playTimeFormatted}</span>
    </div>
  </div>

  <!-- status dot -->
  <span
    class="rounded-full shrink-0"
    style="
      width: {selected ? '7px' : '6px'}; height: {selected ? '7px' : '6px'};
      background: {selected ? 'var(--accent)' : 'rgba(255,255,255,0.13)'};
      box-shadow: {selected ? '0 0 8px var(--accent)' : 'none'};
    "
  ></span>
</button>
