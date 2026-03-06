<script lang="ts">
  import { convertFileSrc, invoke } from "@tauri-apps/api/core";
  import { getSelectedGame, launchGame, getGameStatus } from "../stores/games.svelte";
  import type { Game } from "../stores/games.svelte";
  import { formatPlayTime, formatLastPlayed } from "../utils/format";

  async function openConfig(): Promise<void> {
    if (!game) return;
    try {
      await invoke("open_game_config", { gameId: game.id });
    } catch (e) {
      console.error("Failed to open config:", e);
    }
  }

  async function openLogs(): Promise<void> {
    if (!game) return;
    try {
      await invoke("open_game_logs", { gameId: game.id });
    } catch (e) {
      console.error("Failed to open logs:", e);
    }
  }

  let game: Game | undefined = $derived(getSelectedGame());

  let coverError = $state(false);

  $effect(() => {
    if (game) coverError = false;
  });

  let coverSrc = $derived(
    game?.cover_art_path && !coverError ? convertFileSrc(game.cover_art_path) : null,
  );

  let playTimeFormatted = $derived(formatPlayTime(game?.play_time_minutes ?? 0, true));
  let lastPlayedFormatted = $derived(formatLastPlayed(game?.last_played ?? null));

  let currentStatus = $derived(game ? getGameStatus(game.id) : "idle");
  let launchButtonText = $derived(
    currentStatus === "launching"
      ? "Launching..."
      : currentStatus === "injecting"
        ? "Injecting..."
        : "Launch + Inject",
  );
  let launchDisabled = $derived(currentStatus === "launching" || currentStatus === "injecting");

  let statusLabel = $derived(
    currentStatus === "launching"
      ? "Launching"
      : currentStatus === "injecting"
        ? "Injecting"
        : currentStatus === "error"
          ? "Error"
          : "Ready",
  );
  let statusStyle = $derived(
    currentStatus === "error"
      ? "background: rgba(255, 107, 107, 0.12); color: #ff6b6b; border-color: rgba(255, 107, 107, 0.2);"
      : currentStatus === "launching"
        ? "background: rgba(255, 193, 7, 0.12); color: #ffc107; border-color: rgba(255, 193, 7, 0.2);"
        : currentStatus === "injecting"
          ? "background: rgba(99, 140, 255, 0.15); color: #638cff; border-color: rgba(99, 140, 255, 0.2);"
          : "background: rgba(6, 214, 160, 0.12); color: #06d6a0; border-color: rgba(6, 214, 160, 0.2);",
  );

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
          onerror={() => { coverError = true; }}
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
            style={statusStyle}
          >
            {statusLabel}
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
          class="flex-1 max-w-[280px] py-3.5 px-7 border-none rounded-[10px] text-white font-display text-base font-bold tracking-[2px] uppercase flex items-center justify-center gap-2.5 transition-all duration-300"
          class:cursor-pointer={!launchDisabled}
          class:cursor-not-allowed={launchDisabled}
          class:opacity-60={launchDisabled}
          style="background: linear-gradient(135deg, #638cff 0%, #06d6a0 100%); box-shadow: 0 4px 20px rgba(99, 140, 255, 0.25);"
          disabled={launchDisabled}
          onclick={() => game && launchGame(game.id)}
          title="Launch + Inject"
        >
          <svg
            width="18"
            height="18"
            viewBox="0 0 24 24"
            fill="currentColor"
          >
            <polygon points="5 3 19 12 5 21 5 3" />
          </svg>
          {launchButtonText}
        </button>
        <button
          title="Open config.toml"
          class="py-3.5 px-5 border border-border-subtle rounded-[10px] font-display text-[0.85rem] font-semibold tracking-wider uppercase text-text-secondary transition-all duration-150 cursor-pointer hover:text-text-primary hover:bg-[rgba(255,255,255,0.06)] hover:border-border-glow"
          style="background: rgba(255, 255, 255, 0.03);"
          onclick={openConfig}
        >
          Config
        </button>
        <button
          title="Open companion.log"
          class="py-3.5 px-5 border border-border-subtle rounded-[10px] font-display text-[0.85rem] font-semibold tracking-wider uppercase text-text-secondary transition-all duration-150 cursor-pointer hover:text-text-primary hover:bg-[rgba(255,255,255,0.06)] hover:border-border-glow"
          style="background: rgba(255, 255, 255, 0.03);"
          onclick={openLogs}
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
