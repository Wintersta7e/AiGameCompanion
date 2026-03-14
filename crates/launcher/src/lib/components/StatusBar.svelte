<script lang="ts">
  import { onMount } from "svelte";
  import { getVersion } from "@tauri-apps/api/app";

  interface Props {
    gameCount: number;
  }

  let { gameCount }: Props = $props();

  let version = $state("...");

  onMount(async () => {
    try { version = await getVersion(); } catch { version = "0.1.0"; }
  });
</script>

<footer
  class="h-8 flex items-center justify-between px-5 shrink-0 border-t border-border-subtle"
  style="background: rgba(10, 12, 20, 0.9); backdrop-filter: blur(10px);"
>
  <div class="flex items-center gap-4 text-[0.68rem] text-text-muted font-mono tracking-wide">
    <div class="flex items-center gap-[5px]">
      <span
        class="w-1.5 h-1.5 rounded-full bg-accent-tertiary"
        style="box-shadow: 0 0 6px rgba(6, 214, 160, 0.4);"
      ></span>
      <span>System Ready</span>
    </div>
    <span>{gameCount} {gameCount === 1 ? "game" : "games"}</span>
  </div>
  <div class="flex items-center gap-4 text-[0.68rem] text-text-muted font-mono tracking-wide">
    <span>v{version}</span>
  </div>
</footer>
