<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getVersion } from '@tauri-apps/api/app';
  import { formatHotkeyStatus } from '../utils/format';

  interface Props {
    gameCount: number;
  }

  let { gameCount }: Props = $props();

  let version = $state('…');
  // Hotkeys that failed to register; null until the backend has answered.
  let failedHotkeys = $state<string[] | null>(null);
  let hotkeyDot = $derived(failedHotkeys?.length === 0 ? 'var(--color-ok)' : 'var(--color-err)');

  onMount(async () => {
    try {
      version = await getVersion();
    } catch {
      version = '2.0.0';
    }
    try {
      failedHotkeys = await invoke<string[]>('hotkey_status');
    } catch (err) {
      console.error('Failed to read the hotkey status:', err);
    }
  });
</script>

<footer
  style="background: rgba(9, 9, 11, 0.78); backdrop-filter: blur(10px);"
  class="h-[34px] flex items-center justify-between px-[18px] shrink-0 border-t border-line font-mono text-[10.5px] text-t-lo"
>
  <div class="flex items-center gap-[14px]">
    {#if failedHotkeys}
      <span class="flex items-center gap-1.5">
        <span
          style="background: {hotkeyDot}; box-shadow: 0 0 6px {hotkeyDot};"
          class="w-1.5 h-1.5 rounded-full"
        ></span>
        {formatHotkeyStatus(failedHotkeys)}
      </span>
    {/if}
    <span>{gameCount} {gameCount === 1 ? 'game' : 'games'}</span>
  </div>
  <span>Sage v{version}</span>
</footer>
