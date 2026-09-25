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
  import { loadProvider } from './lib/stores/companion.svelte';

  // The overlay companion loads the same SPA in a second window; branch on label.
  const isOverlay = getCurrentWindow().label === 'overlay';

  // Set when the library file could not be read: the launcher then runs
  // read-only for the whole session.
  let stateError = $state<string | null>(null);

  onMount(async () => {
    if (isOverlay) return;
    void loadProvider();
    // Ask before scanning: a scan in read-only mode would replace the stored
    // library in memory with a list that has no playtime and cannot be saved.
    try {
      stateError = await invoke<string | null>('state_health');
    } catch {
      stateError = null;
    }
    if (stateError) {
      void loadGames();
      return;
    }
    try {
      const settings = await invoke<{ scan_on_startup: boolean }>('get_settings');
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
    <main class="flex flex-1 overflow-hidden">
      <GameList />
      <DetailPanel onOpenSettings={() => (settingsOpen = true)} />
    </main>
    <StatusBar gameCount={games.length} />
    <SettingsModal bind:open={settingsOpen} />
  </div>
{/if}
