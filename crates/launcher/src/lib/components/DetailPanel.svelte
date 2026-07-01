<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { getSelectedGame, launchGame, getGameStatus, type Game } from '../stores/games.svelte';
  import { setAccentFromGame } from '../stores/accent.svelte';
  import { getProviderMeta } from '../stores/companion.svelte';
  import { formatPlayTime, formatLastPlayed } from '../utils/format';

  let game: Game | undefined = $derived(getSelectedGame());
  let prov = $derived(getProviderMeta());

  let coverError = $state(false);
  let fileError = $state<string | null>(null);

  // Cover art drives the accent: recompute whenever the selection changes.
  $effect(() => {
    if (game) {
      coverError = false;
      fileError = null;
      setAccentFromGame(game);
    }
  });

  // Per-source dot colour (matches the sidebar rows).
  const sourceColors: Record<string, string> = {
    steam: '#66c0f4',
    epic: '#cfcfcf',
    gog: '#b035e8',
    manual: 'var(--accent)',
  };
  let srcDot = $derived(game ? (sourceColors[game.source] ?? 'var(--accent)') : 'var(--accent)');

  let coverSrc = $derived(game?.cover_art_path && !coverError ? game.cover_art_path : null);
  let initial = $derived(
    (game?.name ?? '?')
      .replace(/[^A-Za-z0-9]/, '')
      .charAt(0)
      .toUpperCase(),
  );
  let shortName = $derived((game?.name ?? '').split(' ').slice(0, 2).join(' '));
  let playTime = $derived(formatPlayTime(game?.play_time_minutes ?? 0, true));
  let lastPlayed = $derived(formatLastPlayed(game?.last_played ?? null));

  // Status → label/colour, mirroring the launch lifecycle.
  let status = $derived(game ? getGameStatus(game.id) : 'idle');
  let statusLabel = $derived(
    status === 'launching'
      ? 'Launching…'
      : status === 'error'
        ? 'Error'
        : status === 'linked'
          ? 'Linked · Active'
          : 'Ready',
  );
  let statusColor = $derived(
    status === 'error'
      ? 'var(--color-err)'
      : status === 'launching'
        ? 'var(--color-warn)'
        : status === 'linked'
          ? 'var(--color-ok)'
          : 'var(--accent)',
  );
  let launchBusy = $derived(status === 'launching');
  let launchLabel = $derived(
    status === 'launching' ? 'Launching…' : status === 'linked' ? 'Relaunch' : 'Launch',
  );
  let beamLabel = $derived(
    status === 'linked' ? 'LINKED' : launchBusy ? 'LINKING…' : 'CTRL+SHIFT+G ⇄ OVERLAY',
  );

  function capSource(s: string): string {
    return s === 'gog' ? 'GOG' : s.charAt(0).toUpperCase() + s.slice(1);
  }

  async function openConfig() {
    fileError = null;
    try {
      await invoke('open_game_config');
    } catch (e) {
      fileError = String(e);
    }
  }
  async function openLogs() {
    fileError = null;
    try {
      await invoke('open_game_logs');
    } catch (e) {
      fileError = String(e);
    }
  }

  // Hover affordance for the outline buttons (Config/Logs): accent border + bg lift.
  function fileBtnEnter(e: MouseEvent) {
    const el = e.currentTarget as HTMLElement;
    el.style.borderColor = 'color-mix(in oklab, var(--accent) 38%, transparent)';
    el.style.background = 'rgba(255,255,255,0.06)';
  }
  function fileBtnLeave(e: MouseEvent) {
    const el = e.currentTarget as HTMLElement;
    el.style.borderColor = 'var(--color-line)';
    el.style.background = 'rgba(255,255,255,0.03)';
  }
</script>

<div class="flex-1 overflow-y-auto relative" style="background: var(--color-ink-1);">
  {#if game}
    <!-- HERO -->
    <div class="relative h-[296px] overflow-hidden">
      {#if coverSrc}
        <img
          src={coverSrc}
          alt={game.name}
          class="absolute inset-0 w-full h-full object-cover"
          style="filter: brightness(0.72) saturate(1.2);"
          onerror={() => (coverError = true)}
        />
      {:else}
        <div
          class="absolute inset-0 grid place-items-center"
          style="background: linear-gradient(135deg, color-mix(in oklab, var(--accent) 30%, #14141a) 0%, #0d0d10 70%);"
        >
          <span class="font-display text-[6rem] font-bold text-white/10">{initial}</span>
        </div>
      {/if}
      <div
        class="absolute inset-0"
        style="background: radial-gradient(120% 110% at 22% 0%, transparent 0%, rgba(10,10,12,0.4) 58%, var(--color-ink-1) 100%);"
      ></div>
      <div
        class="absolute inset-0"
        style="background: linear-gradient(180deg, rgba(10,10,12,0.05) 0%, rgba(10,10,12,0.5) 52%, var(--color-ink-1) 100%);"
      ></div>
      <div
        class="absolute inset-0"
        style="background: linear-gradient(180deg, transparent 42%, color-mix(in oklab, var(--accent) 14%, transparent) 100%); mix-blend-mode: screen;"
      ></div>

      <div class="absolute inset-x-0 bottom-0 px-9 pb-6 pt-8" style="animation: fade-up 0.5s ease;">
        <div class="flex items-center gap-2 mb-[11px]">
          <span
            class="font-display text-[10px] font-semibold tracking-[0.18em] uppercase"
            style="color: var(--accent);">Companion bound</span
          >
          <span
            class="px-[11px] py-1 rounded-full font-display text-[10px] font-semibold tracking-[0.1em] uppercase"
            style="color: {statusColor}; background: color-mix(in oklab, {statusColor} 15%, transparent); border: 1px solid color-mix(in oklab, {statusColor} 32%, transparent);"
            >{statusLabel}</span
          >
        </div>
        <h1
          class="m-0 font-display text-[34px] font-bold tracking-[0.01em] leading-[1.05] text-white"
          style="text-shadow: 0 2px 24px rgba(0,0,0,0.5);"
        >
          {game.name}
        </h1>
        <div class="flex items-center gap-[13px] mt-[11px]">
          <span class="flex items-center gap-[7px] text-[11.5px] text-t-mid">
            <span
              class="w-[7px] h-[7px] rounded-full"
              style="background: {srcDot}; box-shadow: 0 0 6px {srcDot};"
            ></span>
            {capSource(game.source)}
          </span>
          {#if game.exe_name}<span class="font-mono text-[11px] text-t-lo">{game.exe_name}</span
            >{/if}
        </div>
      </div>
    </div>

    <!-- BODY -->
    <div class="px-9 pt-6 pb-10 flex flex-col gap-6">
      <!-- LINK STATION -->
      <div
        class="relative rounded-2xl border border-line overflow-hidden p-6"
        style="background: linear-gradient(180deg, var(--color-ink-2), var(--color-ink-1)); animation: fade-up 0.5s ease 0.05s both;"
      >
        <div
          class="absolute -top-[46%] left-1/2 -translate-x-1/2 w-[420px] h-[240px] pointer-events-none"
          style="background: radial-gradient(circle, color-mix(in oklab, var(--accent) 26%, transparent), transparent 70%); filter: blur(22px); opacity: 0.45;"
        ></div>

        <div class="relative flex items-center gap-2 mb-[22px]">
          <!-- Sage node -->
          <div class="flex flex-col items-center gap-2.5 w-[128px]">
            <div class="relative w-[54px] h-[54px]">
              <div
                class="absolute inset-0 rounded-full animate-pulse-soft"
                style="background: radial-gradient(circle at 50% 40%, #fff 0%, color-mix(in oklab, var(--accent) 85%, white) 24%, var(--accent) 56%, color-mix(in oklab, var(--accent) 38%, transparent) 80%, transparent 100%); box-shadow: 0 0 28px -2px var(--accent);"
              ></div>
              <div
                class="absolute inset-[3px] rounded-full"
                style="border: 1px solid rgba(255,255,255,0.2);"
              ></div>
              <div
                class="absolute w-2 h-2 rounded-full"
                style="background: rgba(255,255,255,0.92); top: 17%; right: 20%;"
              ></div>
            </div>
            <div class="text-center">
              <div class="font-display text-[12px] font-semibold tracking-[0.16em] text-t-hi">
                SAGE
              </div>
              <div class="flex items-center gap-1.5 justify-center mt-[5px]">
                <span
                  class="w-[7px] h-[7px] rounded-full"
                  style="background: {prov.dot}; box-shadow: 0 0 6px {prov.dot};"
                ></span>
                <span class="text-[10.5px] text-t-lo font-mono">{prov.label}</span>
              </div>
            </div>
          </div>

          <!-- beam -->
          <div class="flex-1 relative h-[54px] flex items-center min-w-[90px]">
            <div
              class="absolute inset-x-0 top-1/2 h-0.5 -translate-y-1/2"
              style="background: linear-gradient(90deg, transparent, color-mix(in oklab, var(--accent) 45%, transparent), transparent);"
            ></div>
            <div
              class="absolute inset-x-0 top-1/2 h-0.5 -translate-y-1/2 animate-beam"
              style="background: linear-gradient(90deg, transparent 0%, var(--accent) 50%, transparent 100%); background-size: 200% 100%; opacity: {launchBusy ||
              status === 'linked'
                ? 1
                : 0.4};"
            ></div>
            <div
              class="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 px-[11px] py-[5px] rounded-full font-mono text-[9px] tracking-[0.1em] whitespace-nowrap"
              style="background: var(--color-ink-3); border: 1px solid color-mix(in oklab, var(--accent) 32%, transparent); color: var(--accent);"
            >
              {beamLabel}
            </div>
          </div>

          <!-- game node -->
          <div class="flex flex-col items-center gap-2.5 w-[128px]">
            <div
              class="relative w-[54px] h-[54px] rounded-[13px] overflow-hidden"
              style="box-shadow: 0 4px 16px rgba(0,0,0,0.5);"
            >
              {#if coverSrc}
                <img src={coverSrc} alt={game.name} class="w-full h-full object-cover" />
              {:else}
                <div
                  class="w-full h-full grid place-items-center font-display font-bold text-[22px] text-white/90"
                  style="background: linear-gradient(135deg, color-mix(in oklab, var(--accent) 45%, #16161a), #101013);"
                >
                  {initial}
                </div>
              {/if}
              <span
                class="absolute inset-0"
                style="box-shadow: inset 0 0 0 1px rgba(255,255,255,0.1);"
              ></span>
            </div>
            <div class="text-center max-w-[128px]">
              <div class="font-display text-[12px] font-medium text-t-hi truncate">{shortName}</div>
              <div class="text-[10px] text-t-lo font-mono mt-1">External · Any API</div>
            </div>
          </div>
        </div>

        <!-- actions -->
        <div class="relative flex items-center gap-3">
          <button
            onclick={() => game && launchGame(game.id)}
            disabled={launchBusy}
            class="flex items-center gap-[9px] px-[26px] py-[13px] rounded-[11px] border-none font-display text-[14px] font-semibold tracking-[0.03em] transition-all duration-200 enabled:hover:brightness-110 enabled:hover:-translate-y-px"
            class:cursor-pointer={!launchBusy}
            class:cursor-not-allowed={launchBusy}
            class:opacity-60={launchBusy}
            style="color: #0b0b0d; background: var(--accent); box-shadow: 0 8px 28px -8px color-mix(in oklab, var(--accent) 75%, transparent);"
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="currentColor"
              ><polygon points="6 4 20 12 6 20 6 4" /></svg
            >
            {launchLabel}
          </button>
          <button
            onclick={openConfig}
            onmouseenter={fileBtnEnter}
            onmouseleave={fileBtnLeave}
            title="Open config.toml"
            class="px-[18px] py-[13px] rounded-[11px] font-display text-[12.5px] font-medium tracking-[0.03em] text-t-mid cursor-pointer transition-all duration-150 hover:text-t-hi"
            style="border: 1px solid var(--color-line); background: rgba(255,255,255,0.03);"
            >Config</button
          >
          <button
            onclick={openLogs}
            onmouseenter={fileBtnEnter}
            onmouseleave={fileBtnLeave}
            title="Open launcher.log"
            class="px-[18px] py-[13px] rounded-[11px] font-display text-[12.5px] font-medium tracking-[0.03em] text-t-mid cursor-pointer transition-all duration-150 hover:text-t-hi"
            style="border: 1px solid var(--color-line); background: rgba(255,255,255,0.03);"
            >Logs</button
          >
          <div class="ml-auto flex items-center gap-[9px]">
            <span
              class="w-2 h-2 rounded-full"
              style="background: {statusColor}; box-shadow: 0 0 9px {statusColor};"
            ></span>
            <span class="text-[12.5px] text-t-mid">{statusLabel}</span>
          </div>
        </div>
      </div>

      {#if fileError}
        <div
          class="px-4 py-2.5 rounded-lg text-[0.82rem]"
          style="background: color-mix(in oklab, var(--color-err) 8%, transparent); border: 1px solid color-mix(in oklab, var(--color-err) 25%, transparent); color: var(--color-err);"
        >
          {fileError}
        </div>
      {/if}

      <!-- STATS -->
      <div class="grid grid-cols-3 gap-3" style="animation: fade-up 0.5s ease 0.12s both;">
        {#each [{ l: 'Play time', v: playTime }, { l: 'Last played', v: lastPlayed }, { l: 'Source', v: capSource(game.source) }] as s (s.l)}
          <div
            class="p-4 rounded-[13px] border border-line"
            style="background: rgba(255,255,255,0.018);"
          >
            <div class="font-mono text-[9.5px] text-t-lo tracking-[0.12em] uppercase mb-[7px]">
              {s.l}
            </div>
            <div class="font-display text-[18px] font-semibold text-t-hi truncate">{s.v}</div>
          </div>
        {/each}
      </div>

      <!-- COMPANION SETUP -->
      <div style="animation: fade-up 0.5s ease 0.18s both;">
        <div class="flex items-center justify-between mb-[13px] pb-2.5 border-b border-line">
          <span class="font-display text-[11px] font-semibold tracking-[0.16em] text-t-lo"
            >COMPANION SETUP</span
          >
          <span class="font-mono text-[10px] text-t-lo">edit in config.toml →</span>
        </div>
        <div class="grid grid-cols-2 gap-2.5">
          <!-- provider -->
          <div
            class="flex items-center justify-between px-[15px] py-[13px] rounded-[11px] border border-line"
            style="background: rgba(255,255,255,0.016);"
          >
            <span class="text-[12.5px] text-t-mid">AI provider</span>
            <span class="flex items-center gap-[7px]"
              ><span
                class="w-[7px] h-[7px] rounded-full"
                style="background: {prov.dot}; box-shadow: 0 0 6px {prov.dot};"
              ></span><span class="font-display text-[12.5px] font-medium text-t-hi"
                >{prov.label}</span
              ></span
            >
          </div>
          <!-- model -->
          <div
            class="flex items-center justify-between px-[15px] py-[13px] rounded-[11px] border border-line"
            style="background: rgba(255,255,255,0.016);"
          >
            <span class="text-[12.5px] text-t-mid">Model</span>
            <span
              class="font-mono text-[11px] px-[9px] py-[3px] rounded-md"
              style="color: var(--accent); background: color-mix(in oklab, var(--accent) 10%, transparent);"
              >{prov.model}</span
            >
          </div>
          <!-- overlay hotkey -->
          <div
            class="flex items-center justify-between px-[15px] py-[13px] rounded-[11px] border border-line"
            style="background: rgba(255,255,255,0.016);"
          >
            <span class="text-[12.5px] text-t-mid">Overlay hotkey</span>
            <span
              class="font-mono text-[11px] text-t-hi px-[9px] py-[3px] rounded-md"
              style="background: var(--color-ink-3); border: 1px solid var(--color-line); box-shadow: 0 1.5px 0 rgba(0,0,0,0.4);"
              >Ctrl+Shift+G</span
            >
          </div>
          <!-- vision -->
          <div
            class="flex items-center justify-between px-[15px] py-[13px] rounded-[11px] border border-line"
            style="background: rgba(255,255,255,0.016);"
          >
            <span class="text-[12.5px] text-t-mid">Screenshot vision</span>
            <span class="font-display text-[12px] font-medium" style="color: var(--accent);"
              >Enabled</span
            >
          </div>
        </div>
      </div>
    </div>
  {:else}
    <div class="flex items-center justify-center h-full">
      <div class="text-center">
        <div class="text-t-mid text-lg font-display font-semibold mb-2">No game selected</div>
        <div class="text-t-lo text-sm">Pick a game from the left to bind Sage.</div>
      </div>
    </div>
  {/if}
</div>
