<script lang="ts">
  import { convertFileSrc } from "@tauri-apps/api/core";
  import { getSelectedGame } from "../stores/games.svelte";
  import type { Game } from "../stores/games.svelte";

  let game: Game | undefined = $derived(getSelectedGame());

  let coverSrc = $derived(
    game?.cover_art_path ? convertFileSrc(game.cover_art_path) : null,
  );

  let playTimeFormatted = $derived(formatPlayTime(game?.play_time_minutes ?? 0));
  let lastPlayedFormatted = $derived(formatLastPlayed(game?.last_played ?? null));

  function formatPlayTime(minutes: number): string {
    if (minutes === 0) return "0h";
    if (minutes < 60) return `${minutes}m`;
    const hours = Math.floor(minutes / 60);
    const mins = minutes % 60;
    if (mins === 0) return `${hours}h`;
    return `${hours}h ${mins}m`;
  }

  function formatLastPlayed(dateStr: string | null): string {
    if (!dateStr) return "Never";
    try {
      const date = new Date(dateStr);
      const now = new Date();
      const diffMs = now.getTime() - date.getTime();
      const diffMins = Math.floor(diffMs / 60000);
      if (diffMins < 1) return "Just now";
      if (diffMins < 60) return `${diffMins} minute${diffMins === 1 ? "" : "s"} ago`;
      const diffHours = Math.floor(diffMins / 60);
      if (diffHours < 24) return `${diffHours} hour${diffHours === 1 ? "" : "s"} ago`;
      const diffDays = Math.floor(diffHours / 24);
      if (diffDays < 30) return `${diffDays} day${diffDays === 1 ? "" : "s"} ago`;
      const diffMonths = Math.floor(diffDays / 30);
      return `${diffMonths} month${diffMonths === 1 ? "" : "s"} ago`;
    } catch {
      return "Unknown";
    }
  }

  function capitalizeSource(source: string): string {
    if (source === "gog") return "GOG";
    return source.charAt(0).toUpperCase() + source.slice(1);
  }
</script>

<div class="flex-1 overflow-y-auto">
  {#if game}
    <!-- Hero section -->
    <div class="relative h-80 overflow-hidden">
      {#if coverSrc}
        <img
          src={coverSrc}
          alt={game.name}
          class="w-full h-full object-cover"
          style="filter: brightness(0.4) saturate(1.2);"
        />
      {:else}
        <div
          class="w-full h-full"
          style="background: linear-gradient(135deg, rgba(99, 140, 255, 0.15) 0%, rgba(168, 85, 247, 0.15) 50%, rgba(10, 12, 20, 1) 100%);"
        ></div>
      {/if}
      <!-- Gradient overlay -->
      <div
        class="absolute inset-0"
        style="background: linear-gradient(180deg, rgba(10, 12, 20, 0.2) 0%, rgba(10, 12, 20, 0.6) 60%, #0a0c14 100%);"
      ></div>
      <!-- Hero content -->
      <div class="absolute bottom-0 left-0 right-0 px-10 pb-6 pt-8 animate-fade-up">
        <h1
          class="font-display text-[2.4rem] font-bold tracking-wide leading-tight"
          style="text-shadow: 0 2px 20px rgba(0, 0, 0, 0.5);"
        >
          {game.name}
        </h1>
        <div class="text-[0.85rem] text-text-secondary mt-2 flex items-center gap-3">
          <span
            class="py-[3px] px-2.5 rounded-full font-display text-[0.7rem] font-semibold tracking-wide uppercase border"
            style="background: rgba(99, 140, 255, 0.15); color: #638cff; border-color: rgba(99, 140, 255, 0.2);"
          >
            {capitalizeSource(game.source)}
          </span>
          <span
            class="py-[3px] px-2.5 rounded-full font-display text-[0.7rem] font-semibold tracking-wide uppercase border"
            style="background: rgba(6, 214, 160, 0.12); color: #06d6a0; border-color: rgba(6, 214, 160, 0.2);"
          >
            Ready
          </span>
          <span class="text-text-secondary">{game.exe_name}</span>
        </div>
      </div>
    </div>

    <!-- Body -->
    <div class="px-10 pt-5 pb-10">
      <!-- Action buttons -->
      <div class="flex gap-3 mb-7 animate-fade-up" style="animation-delay: 0.1s;">
        <button
          class="flex-1 max-w-[280px] py-3.5 px-7 border-none rounded-[10px] text-white font-display text-base font-bold tracking-[2px] uppercase cursor-not-allowed opacity-60 flex items-center justify-center gap-2.5 transition-all duration-300"
          style="background: linear-gradient(135deg, #638cff 0%, #06d6a0 100%); box-shadow: 0 4px 20px rgba(99, 140, 255, 0.25);"
          disabled
          title="Launch + Inject (coming soon)"
        >
          <svg
            width="18"
            height="18"
            viewBox="0 0 24 24"
            fill="currentColor"
          >
            <polygon points="5 3 19 12 5 21 5 3" />
          </svg>
          Launch + Inject
        </button>
        <button
          class="py-3.5 px-5 border border-border-subtle rounded-[10px] font-display text-[0.85rem] font-semibold tracking-wider uppercase cursor-pointer text-text-secondary transition-all duration-150 hover:border-border-glow hover:text-accent hover:bg-[rgba(99,140,255,0.06)]"
          style="background: rgba(255, 255, 255, 0.03);"
        >
          Config
        </button>
        <button
          class="py-3.5 px-5 border border-border-subtle rounded-[10px] font-display text-[0.85rem] font-semibold tracking-wider uppercase cursor-pointer text-text-secondary transition-all duration-150 hover:border-border-glow hover:text-accent hover:bg-[rgba(99,140,255,0.06)]"
          style="background: rgba(255, 255, 255, 0.03);"
        >
          Logs
        </button>
      </div>

      <!-- Stats grid -->
      <div
        class="grid grid-cols-3 gap-3.5 mb-7 animate-fade-up"
        style="animation-delay: 0.2s;"
      >
        <div
          class="p-4 bg-bg-glass border border-border-subtle rounded-[10px]"
          style="backdrop-filter: blur(10px);"
        >
          <div class="text-[0.68rem] font-semibold text-text-muted uppercase tracking-[1.2px] mb-1.5">
            Play Time
          </div>
          <div class="font-display text-base font-semibold text-text-primary">
            {playTimeFormatted}
          </div>
        </div>
        <div
          class="p-4 bg-bg-glass border border-border-subtle rounded-[10px]"
          style="backdrop-filter: blur(10px);"
        >
          <div class="text-[0.68rem] font-semibold text-text-muted uppercase tracking-[1.2px] mb-1.5">
            Last Played
          </div>
          <div class="font-display text-base font-semibold text-text-primary">
            {lastPlayedFormatted}
          </div>
        </div>
        <div
          class="p-4 bg-bg-glass border border-border-subtle rounded-[10px]"
          style="backdrop-filter: blur(10px);"
        >
          <div class="text-[0.68rem] font-semibold text-text-muted uppercase tracking-[1.2px] mb-1.5">
            Graphics API
          </div>
          <div class="font-display text-base font-semibold text-text-primary">
            Auto
          </div>
        </div>
      </div>

      <!-- Companion Settings -->
      <div class="animate-fade-up" style="animation-delay: 0.3s;">
        <div
          class="font-display text-[0.8rem] font-bold text-text-muted uppercase tracking-[1.5px] mb-3 pb-2 border-b border-border-subtle"
        >
          Companion Settings
        </div>
        <div class="flex flex-col gap-2.5">
          <div
            class="flex items-center justify-between py-2.5 px-3.5 border border-border-subtle rounded-md"
            style="background: rgba(255, 255, 255, 0.02);"
          >
            <span class="text-[0.85rem] text-text-secondary">Overlay Hotkey</span>
            <span
              class="font-mono text-[0.78rem] text-accent py-[3px] px-2.5 rounded"
              style="background: rgba(99, 140, 255, 0.08);"
            >
              F9
            </span>
          </div>
          <div
            class="flex items-center justify-between py-2.5 px-3.5 border border-border-subtle rounded-md"
            style="background: rgba(255, 255, 255, 0.02);"
          >
            <span class="text-[0.85rem] text-text-secondary">Translate Hotkey</span>
            <span
              class="font-mono text-[0.78rem] text-accent py-[3px] px-2.5 rounded"
              style="background: rgba(99, 140, 255, 0.08);"
            >
              F10
            </span>
          </div>
          <div
            class="flex items-center justify-between py-2.5 px-3.5 border border-border-subtle rounded-md"
            style="background: rgba(255, 255, 255, 0.02);"
          >
            <span class="text-[0.85rem] text-text-secondary">AI Model</span>
            <span
              class="font-mono text-[0.78rem] text-accent py-[3px] px-2.5 rounded"
              style="background: rgba(99, 140, 255, 0.08);"
            >
              gemini-2.5-flash
            </span>
          </div>
          <div
            class="flex items-center justify-between py-2.5 px-3.5 border border-border-subtle rounded-md"
            style="background: rgba(255, 255, 255, 0.02);"
          >
            <span class="text-[0.85rem] text-text-secondary">Translation</span>
            <span
              class="font-mono text-[0.78rem] text-accent py-[3px] px-2.5 rounded"
              style="background: rgba(99, 140, 255, 0.08);"
            >
              Gemini -- English
            </span>
          </div>
        </div>
      </div>
    </div>
  {:else}
    <!-- Empty state -->
    <div class="flex items-center justify-center h-full">
      <div class="text-center">
        <div class="text-text-muted text-lg font-display font-semibold mb-2">
          No Game Selected
        </div>
        <div class="text-text-muted text-sm">
          Select a game to get started
        </div>
      </div>
    </div>
  {/if}
</div>
