<script lang="ts">
  import type { Game } from '../stores/games.svelte';
  import { setSelectedGameId } from '../stores/games.svelte';
  import { formatPlayTime } from '../utils/format';
  import { hashHue } from '../utils/accent';

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
  // Remembering which URL failed (rather than a boolean reset by an effect)
  // means a different game's cover is always retried, and the same broken one
  // never is.
  let failedCover = $state<string | null>(null);
  let imgError = $derived(coverSrc !== null && failedCover === coverSrc);

  let playTimeFormatted = $derived(formatPlayTime(game.play_time_minutes));
  let initial = $derived(
    game.name
      .replace(/[^A-Za-z0-9]/, '')
      .charAt(0)
      .toUpperCase(),
  );
  let dotColor = $derived(sourceColors[game.source] ?? 'var(--accent)');
  // Each row's initial-fallback thumb gets its own hue (the global --accent is
  // reserved for the selected row's rail/border/dot), so the placeholder thumbs
  // stay multi-coloured like the design instead of all sharing one colour.
  let thumbColor = $derived(hashHue(game.id));
</script>

<button
  style="
    animation: card-in 0.3s ease-out {index * 0.03}s both;
    border: 1px solid {selected
    ? 'color-mix(in oklab, var(--accent) 34%, transparent)'
    : 'transparent'};
    background: {selected
    ? 'linear-gradient(90deg, color-mix(in oklab, var(--accent) 16%, transparent), transparent 62%)'
    : 'transparent'};
  "
  class="relative flex items-center gap-[11px] w-full text-left px-[11px] py-[9px] rounded-[11px] cursor-pointer transition-all duration-150"
  onclick={() => {
    setSelectedGameId(game.id);
  }}
  onmouseenter={(e) => {
    if (!selected) (e.currentTarget as HTMLElement).style.background = 'rgba(255,255,255,0.035)';
  }}
  onmouseleave={(e) => {
    if (!selected) (e.currentTarget as HTMLElement).style.background = 'transparent';
  }}
  type="button"
>
  <!-- accent rail (selected) -->
  <span
    style="height: {selected ? '28px' : '0'}; background: var(--accent);"
    class="absolute left-0 top-1/2 -translate-y-1/2 w-[3px] rounded-r-[3px] transition-all duration-200"
  ></span>

  <!-- cover thumb -->
  <div
    style="box-shadow: 0 2px 9px rgba(0,0,0,0.45);"
    class="relative w-[38px] h-[52px] rounded-[7px] shrink-0 overflow-hidden"
  >
    {#if coverSrc && !imgError}
      <img
        class="w-full h-full object-cover"
        alt={game.name}
        loading="lazy"
        onerror={() => (failedCover = coverSrc)}
        src={coverSrc}
      />
    {:else}
      <div
        style="background: linear-gradient(135deg, color-mix(in oklab, {thumbColor} 45%, #16161a), #101013); text-shadow: 0 1px 5px rgba(0,0,0,0.55);"
        class="w-full h-full grid place-items-center font-display font-bold text-base text-white/85"
      >
        {initial}
      </div>
    {/if}
    <span style="box-shadow: inset 0 0 0 1px rgba(255,255,255,0.08);" class="absolute inset-0"
    ></span>
  </div>

  <!-- info -->
  <div class="flex-1 min-w-0">
    <div class="font-display text-[13px] font-medium text-t-hi truncate">{game.name}</div>
    <div class="flex items-center gap-[7px] mt-[3px] text-[10.5px] text-t-lo">
      <span
        style="background: {dotColor}; box-shadow: 0 0 6px color-mix(in oklab, {dotColor} 60%, transparent);"
        class="w-[7px] h-[7px] rounded-full shrink-0"
      ></span>
      <span class="uppercase tracking-wide">{game.source}</span>
      <span class="opacity-50">·</span>
      <span class="font-mono">{playTimeFormatted}</span>
    </div>
  </div>

  <!-- status dot -->
  <span
    style="
      width: {selected ? '7px' : '6px'}; height: {selected ? '7px' : '6px'};
      background: {selected ? 'var(--accent)' : 'rgba(255,255,255,0.13)'};
      box-shadow: {selected ? '0 0 8px var(--accent)' : 'none'};
    "
    class="rounded-full shrink-0"
  ></span>
</button>
