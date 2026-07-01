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

  onMount(async () => {
    if (isOverlay) return;
    void loadProvider();
    try {
      const settings = await invoke<{ scan_on_startup: boolean }>('get_settings');
      if (settings.scan_on_startup) scanGames();
      else loadGames();
    } catch {
      scanGames();
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
    <main class="flex flex-1 overflow-hidden">
      <GameList />
      <DetailPanel onOpenSettings={() => (settingsOpen = true)} />
    </main>
    <StatusBar gameCount={games.length} />
    <SettingsModal bind:open={settingsOpen} />
  </div>
{/if}
