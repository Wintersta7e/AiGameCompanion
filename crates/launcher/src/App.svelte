<script lang="ts">
  import { onMount } from "svelte";
  import TopBar from "./lib/components/TopBar.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import GameList from "./lib/components/GameList.svelte";
  import DetailPanel from "./lib/components/DetailPanel.svelte";
  import Background from "./lib/components/Background.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { scanGames, getGames, loadGames } from "./lib/stores/games.svelte";

  onMount(async () => {
    try {
      const settings = await invoke<{ scan_on_startup: boolean }>("get_settings");
      if (settings.scan_on_startup) {
        scanGames();
      } else {
        loadGames();
      }
    } catch {
      // Fallback: scan if settings can't be loaded
      scanGames();
    }
  });

  let games = $derived(getGames());
  let settingsOpen = $state(false);
</script>

<Background />
<div class="relative z-10 flex flex-col h-screen">
  <TopBar onOpenSettings={() => (settingsOpen = true)} />
  <main class="flex flex-1 overflow-hidden">
    <GameList />
    <DetailPanel />
  </main>
  <StatusBar gameCount={games.length} />
</div>
<SettingsModal bind:open={settingsOpen} />
