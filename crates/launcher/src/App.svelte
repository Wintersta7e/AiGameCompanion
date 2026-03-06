<script lang="ts">
  import { onMount } from "svelte";
  import TopBar from "./lib/components/TopBar.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import GameList from "./lib/components/GameList.svelte";
  import DetailPanel from "./lib/components/DetailPanel.svelte";
  import Background from "./lib/components/Background.svelte";
  import SettingsModal from "./lib/components/SettingsModal.svelte";
  import { scanGames, getGames } from "./lib/stores/games.svelte";

  onMount(() => {
    scanGames();
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
