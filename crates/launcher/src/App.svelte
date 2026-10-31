<script lang="ts">
  import { onMount } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { invoke } from '@tauri-apps/api/core';
  import TopBar from './lib/components/TopBar.svelte';
  import StatusBar from './lib/components/StatusBar.svelte';
  import GameList from './lib/components/GameList.svelte';
  import DetailPanel from './lib/components/DetailPanel.svelte';
  import Background from './lib/components/Background.svelte';
  import SettingsModal from './lib/components/SettingsModal.svelte';
  import Overlay from './lib/components/Overlay.svelte';
  import { scanGames, getGames, loadGames } from './lib/stores/games.svelte';
  import { followProviderChanges, loadProvider } from './lib/stores/companion.svelte';
  import type { LauncherSettings } from './lib/settings';

  // The overlay companion loads the same SPA in a second window; branch on label.
  const isOverlay = getCurrentWindow().label === 'overlay';

  // Set when the library file could not be read: the launcher then runs
  // read-only for the whole session.
  let stateError = $state<string | null>(null);
  // Set when part or all of the library file could not be read but a copy of
  // it was kept; says where. The backend hands it out once per run.
  let stateNotice = $state<string | null>(null);

  onMount(async () => {
    if (isOverlay) return;
    followProviderChanges();
    void loadProvider();
    // Ask before scanning: a scan in read-only mode would replace the stored
    // library in memory with a list that has no playtime and cannot be saved.
    try {
      const health = await invoke<{ read_only: string | null; notice: string | null }>(
        'state_health',
      );
      stateError = health.read_only;
      stateNotice = health.notice;
    } catch {
      stateError = null;
    }
    if (stateError) {
      void loadGames();
      return;
    }
    try {
      const settings = await invoke<LauncherSettings>('get_settings');
      if (settings.scan_on_startup) void scanGames();
      else void loadGames();
    } catch {
      void scanGames();
    }
  });

  let games = $derived(getGames());
  let settingsOpen = $state(false);
</script>

{#if isOverlay}
  <Overlay />
{:else}
  <Background />
  <div class="relative z-10 flex flex-col h-screen">
    <TopBar onOpenSettings={() => (settingsOpen = true)} />
    {#if stateError}
      <div
        style="background: color-mix(in oklab, var(--color-err) 12%, var(--color-ink-1));"
        class="shrink-0 px-[18px] py-2.5 border-b border-line"
        role="alert"
      >
        <div class="text-err text-sm font-display">
          Your game library could not be read, so nothing will be saved this session. Restart the
          launcher to try again.
        </div>
        <div class="text-t-lo text-xs font-mono mt-1 break-all">{stateError}</div>
      </div>
    {/if}
    {#if stateNotice}
      <div
        style="background: color-mix(in oklab, var(--color-warn) 10%, var(--color-ink-1));"
        class="shrink-0 px-[18px] py-2.5 border-b border-line flex items-start gap-3"
        role="status"
      >
        <div class="flex-1 text-warn text-sm font-display break-words">{stateNotice}</div>
        <button
          class="shrink-0 text-t-mid hover:text-t-hi text-xs"
          onclick={() => (stateNotice = null)}
          type="button"
        >
          Dismiss
        </button>
      </div>
    {/if}
    <main class="flex flex-1 overflow-hidden">
      <GameList />
      <DetailPanel onOpenSettings={() => (settingsOpen = true)} />
    </main>
    <StatusBar gameCount={games.length} />
    <SettingsModal bind:open={settingsOpen} />
  </div>
{/if}
