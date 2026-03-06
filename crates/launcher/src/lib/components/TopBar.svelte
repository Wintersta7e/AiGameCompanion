<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { getSearchQuery, setSearchQuery } from "../stores/games.svelte";

  let { onOpenSettings }: { onOpenSettings: () => void } = $props();

  const appWindow = getCurrentWindow();

  let searchFocused = $state(false);
  let currentQuery = $derived(getSearchQuery());

  let debounceTimer: ReturnType<typeof setTimeout> | null = null;

  function handleInput(e: Event): void {
    const target = e.target as HTMLInputElement;
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      setSearchQuery(target.value);
    }, 150);
  }

  async function minimize(): Promise<void> {
    try { await appWindow.minimize(); } catch (e) { console.error("Minimize failed:", e); }
  }

  async function toggleMaximize(): Promise<void> {
    try { await appWindow.toggleMaximize(); } catch (e) { console.error("Maximize failed:", e); }
  }

  async function close(): Promise<void> {
    try { await appWindow.close(); } catch (e) { console.error("Close failed:", e); }
  }
</script>

<header
  data-tauri-drag-region
  class="flex items-center justify-between h-14 px-6 shrink-0 border-b border-border-subtle"
  style="background: rgba(10, 12, 20, 0.85); backdrop-filter: blur(20px);"
>
  <!-- Left: Logo + Title -->
  <div class="flex items-center gap-3.5">
    <div
      class="w-8 h-8 rounded-lg flex items-center justify-center font-display font-bold text-base text-white"
      style="background: linear-gradient(135deg, #638cff 0%, #a855f7 100%); box-shadow: 0 0 16px rgba(99, 140, 255, 0.3);"
    >
      S
    </div>
    <span
      class="font-display text-xl font-semibold tracking-wider uppercase"
      style="background: linear-gradient(135deg, #638cff 0%, #a855f7 100%); -webkit-background-clip: text; -webkit-text-fill-color: transparent; background-clip: text;"
    >
      AI Game Companion
    </span>
  </div>

  <!-- Center: reserved -->
  <div></div>

  <!-- Right: Search + Settings -->
  <div class="flex items-center gap-2.5">
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      role="search"
      onmousedown={(e) => e.stopPropagation()}
      class="flex items-center gap-2 py-[7px] px-3.5 rounded-[10px] border transition-all duration-300"
      class:w-[220px]={!searchFocused}
      class:w-[280px]={searchFocused}
      style="background: {searchFocused ? 'rgba(255, 255, 255, 0.06)' : 'rgba(255, 255, 255, 0.04)'}; border-color: {searchFocused ? 'rgba(99, 140, 255, 0.25)' : 'rgba(99, 140, 255, 0.08)'}; box-shadow: {searchFocused ? '0 0 16px rgba(99, 140, 255, 0.08)' : 'none'};"
    >
      <svg
        width="14"
        height="14"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2.5"
        stroke-linecap="round"
        class="shrink-0 text-text-muted"
      >
        <circle cx="11" cy="11" r="8" />
        <line x1="21" y1="21" x2="16.65" y2="16.65" />
      </svg>
      <input
        type="text"
        placeholder="Search games..."
        value={currentQuery}
        oninput={handleInput}
        onfocus={() => (searchFocused = true)}
        onblur={() => (searchFocused = false)}
        aria-label="Search games"
        class="bg-transparent border-none outline-none text-text-primary font-body text-[0.85rem] w-full placeholder:text-text-muted"
      />
    </div>

    <button
      class="w-9 h-9 flex items-center justify-center border border-border-subtle rounded-md text-text-secondary transition-all duration-150 cursor-pointer hover:text-text-primary hover:bg-[rgba(255,255,255,0.08)] hover:border-border-glow"
      style="background: rgba(255, 255, 255, 0.03);"
      title="Settings"
      aria-label="Settings"
      onclick={onOpenSettings}
    >
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
      >
        <circle cx="12" cy="12" r="3" />
        <path
          d="M19.4 15a1.65 1.65 0 00.33 1.82l.06.06a2 2 0 010 2.83 2 2 0 01-2.83 0l-.06-.06a1.65 1.65 0 00-1.82-.33 1.65 1.65 0 00-1 1.51V21a2 2 0 01-4 0v-.09A1.65 1.65 0 009 19.4a1.65 1.65 0 00-1.82.33l-.06.06a2 2 0 01-2.83-2.83l.06-.06A1.65 1.65 0 004.68 15a1.65 1.65 0 00-1.51-1H3a2 2 0 010-4h.09A1.65 1.65 0 004.6 9a1.65 1.65 0 00-.33-1.82l-.06-.06a2 2 0 012.83-2.83l.06.06A1.65 1.65 0 009 4.68a1.65 1.65 0 001-1.51V3a2 2 0 014 0v.09a1.65 1.65 0 001 1.51 1.65 1.65 0 001.82-.33l.06-.06a2 2 0 012.83 2.83l-.06.06A1.65 1.65 0 0019.4 9a1.65 1.65 0 001.51 1H21a2 2 0 010 4h-.09a1.65 1.65 0 00-1.51 1z"
        />
      </svg>
    </button>

    <!-- Window divider -->
    <div class="w-px h-5 bg-border-subtle mx-1"></div>

    <!-- Window controls -->
    <button
      class="w-8 h-8 flex items-center justify-center rounded-md cursor-pointer text-text-secondary transition-all duration-150 hover:text-text-primary hover:bg-[rgba(255,255,255,0.08)]"
      onclick={minimize}
      title="Minimize"
      aria-label="Minimize"
    >
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
        <line x1="2" y1="6" x2="10" y2="6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
      </svg>
    </button>

    <button
      class="w-8 h-8 flex items-center justify-center rounded-md cursor-pointer text-text-secondary transition-all duration-150 hover:text-text-primary hover:bg-[rgba(255,255,255,0.08)]"
      onclick={toggleMaximize}
      title="Maximize"
      aria-label="Maximize"
    >
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
        <rect x="2" y="2" width="8" height="8" rx="1" stroke="currentColor" stroke-width="1.5" fill="none" />
      </svg>
    </button>

    <button
      class="w-8 h-8 flex items-center justify-center rounded-md cursor-pointer text-text-secondary transition-all duration-150 hover:text-white hover:bg-[rgba(255,60,60,0.7)]"
      onclick={close}
      title="Close"
      aria-label="Close"
    >
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
        <line x1="2" y1="2" x2="10" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
        <line x1="10" y1="2" x2="2" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
      </svg>
    </button>
  </div>
</header>
