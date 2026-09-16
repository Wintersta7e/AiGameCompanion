<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { getGames } from '../stores/games.svelte';
  import { PROVIDERS, getProvider, setProvider, type Provider } from '../stores/companion.svelte';

  let { onOpenSettings }: { onOpenSettings: () => void } = $props();

  const appWindow = getCurrentWindow();

  let provider = $derived(getProvider());
  let count = $derived(getGames().length);

  const providerKeys = Object.keys(PROVIDERS) as Provider[];

  async function minimize() {
    try {
      await appWindow.minimize();
    } catch (e) {
      console.error(e);
    }
  }
  async function toggleMax() {
    try {
      await appWindow.toggleMaximize();
    } catch (e) {
      console.error(e);
    }
  }
  async function close() {
    try {
      await appWindow.close();
    } catch (e) {
      console.error(e);
    }
  }

  // Gear hover: accent-tinted border + subtle bg lift (inline styles win over
  // Tailwind hover utilities, so drive it from JS like the other controls).
  function gearEnter(e: MouseEvent) {
    const el = e.currentTarget as HTMLElement;
    el.style.borderColor = 'color-mix(in oklab, var(--accent) 40%, transparent)';
    el.style.background = 'rgba(255,255,255,0.06)';
  }
  function gearLeave(e: MouseEvent) {
    const el = e.currentTarget as HTMLElement;
    el.style.borderColor = 'var(--color-line)';
    el.style.background = 'rgba(255,255,255,0.03)';
  }
</script>

<header
  style="background: rgba(9, 9, 11, 0.72); backdrop-filter: blur(20px);"
  class="h-[60px] shrink-0 flex items-center justify-between px-4 border-b border-line relative z-10"
  data-tauri-drag-region
>
  <!-- brand -->
  <div class="flex items-center gap-[11px]">
    <div class="relative w-[30px] h-[30px] shrink-0">
      <div
        style="background: radial-gradient(circle at 50% 42%, #fff 0%, color-mix(in oklab, var(--accent) 85%, white) 24%, var(--accent) 56%, color-mix(in oklab, var(--accent) 38%, transparent) 80%, transparent 100%); box-shadow: 0 0 18px -2px var(--accent);"
        class="absolute inset-0 rounded-full animate-pulse-soft"
      ></div>
      <div
        style="border: 1px solid rgba(255,255,255,0.18);"
        class="absolute inset-[2px] rounded-full"
      ></div>
      <div
        style="background: rgba(255,255,255,0.92); top: 18%; right: 20%;"
        class="absolute w-[5px] h-[5px] rounded-full"
      ></div>
    </div>
    <div class="flex flex-col leading-none">
      <span class="font-display font-semibold text-base tracking-[0.16em] text-t-hi">SAGE</span>
      <span class="text-[9px] tracking-[0.34em] text-t-lo mt-1 font-semibold">GAME COMPANION</span>
    </div>
  </div>

  <!-- watcher status -->
  <div
    style="background: rgba(255,255,255,0.025); border: 1px solid var(--color-line);"
    class="flex items-center gap-[9px] px-[15px] py-[7px] rounded-full whitespace-nowrap"
  >
    <span class="relative flex w-[7px] h-[7px]">
      <span
        style="background: var(--color-ok); box-shadow: 0 0 8px var(--color-ok);"
        class="absolute inset-0 rounded-full animate-pulse-fast"
      ></span>
    </span>
    <span class="text-[11.5px] text-t-mid">Watcher active</span>
    <span class="w-px h-[11px] bg-line"></span>
    <span class="font-mono text-[10.5px] text-t-lo">{count} bound · listening</span>
  </div>

  <!-- right cluster -->
  <div class="flex items-center gap-[10px]">
    <!-- provider switch -->
    <div
      style="background: rgba(255,255,255,0.03); border: 1px solid var(--color-line);"
      class="flex items-center gap-[3px] p-[3px] rounded-[11px]"
    >
      {#each providerKeys as key (key)}
        {const active = $derived(key === provider)}
        <button
          style="
            border: 1px solid {active
            ? 'color-mix(in oklab, var(--accent) 32%, transparent)'
            : 'transparent'};
            background: {active
            ? 'color-mix(in oklab, var(--accent) 20%, transparent)'
            : 'transparent'};
            color: {active ? 'var(--color-t-hi)' : 'var(--color-t-lo)'};
          "
          class="flex items-center gap-1.5 px-[11px] py-1.5 rounded-lg font-display text-[11.5px] font-medium tracking-[0.02em] cursor-pointer transition-all duration-150"
          aria-pressed={active}
          onclick={() => {
            setProvider(key);
          }}
          onmouseenter={(e) => {
            if (!active) (e.currentTarget as HTMLElement).style.color = 'var(--color-t-mid)';
          }}
          onmouseleave={(e) => {
            if (!active) (e.currentTarget as HTMLElement).style.color = 'var(--color-t-lo)';
          }}
          type="button"
        >
          <span
            style="background: {PROVIDERS[key].dot}; box-shadow: 0 0 6px {PROVIDERS[key].dot};"
            class="w-[7px] h-[7px] rounded-full"
          ></span>
          {PROVIDERS[key].label}
        </button>
      {/each}
    </div>

    <!-- settings -->
    <button
      style="border: 1px solid var(--color-line); background: rgba(255,255,255,0.03);"
      class="w-[34px] h-[34px] grid place-items-center rounded-[9px] text-t-mid cursor-pointer transition-all duration-150 hover:text-t-hi"
      aria-label="Settings"
      onclick={onOpenSettings}
      onmouseenter={gearEnter}
      onmouseleave={gearLeave}
      title="Settings"
      type="button"
    >
      <svg
        fill="none"
        height="16"
        stroke="currentColor"
        stroke-linecap="round"
        stroke-width="1.8"
        viewBox="0 0 24 24"
        width="16"
      >
        <circle cx="12" cy="12" r="3" />
        <path
          d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 010 2.83 2 2 0 01-2.83 0l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06A1.65 1.65 0 004.68 15a1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06A1.65 1.65 0 009 4.68a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06A1.65 1.65 0 0019.4 9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z"
        />
      </svg>
    </button>

    <span class="w-px h-5 bg-line mx-0.5"></span>

    <!-- window controls -->
    <button
      class="w-[30px] h-[30px] grid place-items-center rounded-lg text-t-mid cursor-pointer transition-all duration-150 hover:text-t-hi hover:bg-white/[0.08]"
      aria-label="Minimize"
      onclick={minimize}
      title="Minimize"
      type="button"
    >
      <svg height="11" viewBox="0 0 12 12" width="11"
        ><line
          stroke="currentColor"
          stroke-linecap="round"
          stroke-width="1.4"
          x1="2"
          x2="10"
          y1="6"
          y2="6"
        /></svg
      >
    </button>
    <button
      class="w-[30px] h-[30px] grid place-items-center rounded-lg text-t-mid cursor-pointer transition-all duration-150 hover:text-t-hi hover:bg-white/[0.08]"
      aria-label="Maximize"
      onclick={toggleMax}
      title="Maximize"
      type="button"
    >
      <svg height="11" viewBox="0 0 12 12" width="11"
        ><rect
          fill="none"
          height="7.6"
          rx="1.4"
          stroke="currentColor"
          stroke-width="1.3"
          width="7.6"
          x="2.2"
          y="2.2"
        /></svg
      >
    </button>
    <button
      style="--hover: rgba(232,72,72,0.75);"
      class="w-[30px] h-[30px] grid place-items-center rounded-lg text-t-mid cursor-pointer transition-all duration-150 hover:text-white"
      aria-label="Close"
      onclick={close}
      onmouseenter={(e) =>
        ((e.currentTarget as HTMLElement).style.background = 'rgba(232,72,72,0.75)')}
      onmouseleave={(e) => ((e.currentTarget as HTMLElement).style.background = 'transparent')}
      title="Close"
      type="button"
    >
      <svg height="11" viewBox="0 0 12 12" width="11"
        ><line
          stroke="currentColor"
          stroke-linecap="round"
          stroke-width="1.4"
          x1="2.4"
          x2="9.6"
          y1="2.4"
          y2="9.6"
        /><line
          stroke="currentColor"
          stroke-linecap="round"
          stroke-width="1.4"
          x1="9.6"
          x2="2.4"
          y1="2.4"
          y2="9.6"
        /></svg
      >
    </button>
  </div>
</header>
