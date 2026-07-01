<script lang="ts">
  import { onMount } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';

  interface Props {
    gameCount: number;
  }

  let { gameCount }: Props = $props();

  let version = $state('…');

  onMount(async () => {
    try {
      version = await getVersion();
    } catch {
      version = '0.6.0';
    }
  });
</script>

<footer
  class="h-[34px] flex items-center justify-between px-[18px] shrink-0 border-t border-line font-mono text-[10.5px] text-t-lo"
  style="background: rgba(9, 9, 11, 0.78); backdrop-filter: blur(10px);"
>
  <div class="flex items-center gap-[14px]">
    <span class="flex items-center gap-1.5">
      <span
        class="w-1.5 h-1.5 rounded-full"
        style="background: var(--color-ok); box-shadow: 0 0 6px var(--color-ok);"
      ></span>
      Watcher active
    </span>
    <span>{gameCount} {gameCount === 1 ? 'game' : 'games'}</span>
  </div>
  <span>Sage v{version}</span>
</footer>
