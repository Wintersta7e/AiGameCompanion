<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { PROVIDERS, getProvider, setProvider, type Provider } from '../stores/companion.svelte';

  type Availability = {
    gemini: boolean;
    claude: boolean;
    openai: boolean;
    claude_where: string;
    openai_where: string;
  };
  interface Settings {
    scan_on_startup: boolean;
    minimize_to_tray: boolean;
    launch_on_startup: boolean;
    active_provider?: string;
  }

  let { open = $bindable(false) }: { open: boolean } = $props();

  const VERSION = 'v2.0.0'; // keep in sync with tauri.conf.json "version"
  const KEY_URL = 'https://aistudio.google.com/apikey';

  let section = $state<'providers' | 'hotkeys' | 'launcher' | 'about'>('providers');
  let settings = $state<Settings>({
    scan_on_startup: true,
    minimize_to_tray: true,
    launch_on_startup: false,
  });
  let availability = $state<Availability>({
    gemini: false,
    claude: false,
    openai: false,
    claude_where: '',
    openai_where: '',
  });
  let provider = $derived(getProvider());
  let geminiKey = $state('');
  let revealKey = $state(false);
  let keySaving = $state(false);
  let rechecking = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  const NAV: { key: typeof section; label: string }[] = [
    { key: 'providers', label: 'Providers' },
    { key: 'hotkeys', label: 'Hotkeys' },
    { key: 'launcher', label: 'Launcher' },
    { key: 'about', label: 'About' },
  ];
  const HOTKEYS = [
    { title: 'Toggle overlay', sub: 'Show or hide Sage over the game', keys: 'G' },
    { title: 'Translate screen', sub: 'Capture and translate on-screen text', keys: 'T' },
    { title: 'Quick ask', sub: 'Screenshot + ask your preset question', keys: 'A' },
  ];
  const TOGGLES: { key: keyof Settings; label: string; sub: string }[] = [
    { key: 'scan_on_startup', label: 'Scan games on startup', sub: 'Refresh the library when Sage launches' },
    { key: 'minimize_to_tray', label: 'Minimize to tray', sub: 'Keep watching for games in the background' },
    { key: 'launch_on_startup', label: 'Launch on system startup', sub: 'Start Sage when Windows boots' },
  ];

  async function load() {
    try {
      settings = await invoke<Settings>('get_settings');
    } catch (e) {
      console.error('settings load failed:', e);
    }
    try {
      availability = await invoke<Availability>('available_providers');
    } catch (e) {
      console.error('availability load failed:', e);
    }
  }

  async function saveKey() {
    const key = geminiKey.trim();
    if (!key || keySaving) return;
    keySaving = true;
    saveError = null;
    try {
      availability = await invoke<Availability>('set_gemini_key', { key });
      geminiKey = '';
      revealKey = false;
    } catch (e) {
      saveError = String(e);
    } finally {
      keySaving = false;
    }
  }

  async function recheck() {
    if (rechecking) return;
    rechecking = true;
    saveError = null;
    try {
      availability = await invoke<Availability>('recheck_clis');
    } catch (e) {
      saveError = String(e);
    } finally {
      rechecking = false;
    }
  }

  async function save() {
    saving = true;
    saveError = null;
    try {
      await invoke('update_settings', { settings });
      open = false;
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  function openUrl(url: string) {
    void invoke('open_url', { url }).catch(() => {});
  }
  function openConfigFolder() {
    void invoke('open_config_folder').catch(() => {});
  }
  function openLogs() {
    void invoke('open_game_logs').catch(() => {});
  }

  function pickProvider(p: Provider) {
    if (!availability[p]) return;
    setProvider(p);
    // Keep the local settings in sync so Save doesn't write back the stale,
    // open-time provider and revert this choice.
    settings.active_provider = p;
  }

  $effect(() => {
    if (open) {
      section = 'providers';
      load();
    }
  });

  function onBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) open = false;
  }
  function onKeydown(e: KeyboardEvent) {
    if (open && e.key === 'Escape') open = false;
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div
    role="dialog"
    aria-modal="true"
    aria-label="Settings"
    class="absolute inset-0 z-[60] flex items-center justify-center"
    onclick={onBackdrop}
    onkeydown={onKeydown}
    style="background: rgba(6, 6, 8, 0.62); backdrop-filter: blur(6px);"
  >
    <div
      class="w-[680px] max-w-[92%] h-[560px] max-h-[92%] rounded-2xl border border-line overflow-hidden flex flex-col"
      style="background: var(--color-ink-1); box-shadow: 0 30px 70px rgba(0,0,0,0.6);"
    >
      <!-- header -->
      <div class="flex items-center gap-3 px-[22px] py-[15px] border-b border-line">
        <span class="relative w-[22px] h-[22px] rounded-full shrink-0" style="background: radial-gradient(circle at 50% 38%, #fff 0%, color-mix(in oklab, var(--accent) 85%, white) 26%, var(--accent) 60%, transparent 100%); box-shadow: 0 0 14px -3px var(--accent);"></span>
        <span class="font-display text-[15px] font-semibold tracking-[0.04em] text-t-hi">Settings</span>
        <span class="font-mono text-[9px] text-t-lo px-[6px] py-[2px] rounded border border-line leading-tight">SAGE · {VERSION}</span>
        <button
          onclick={() => (open = false)}
          aria-label="Close settings"
          class="ml-auto w-[30px] h-[30px] grid place-items-center rounded-lg text-t-mid cursor-pointer transition-colors duration-150 hover:text-white hover:bg-white/[0.06]"
        >
          <svg width="12" height="12" viewBox="0 0 12 12"><line x1="2.4" y1="2.4" x2="9.6" y2="9.6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" /><line x1="9.6" y1="2.4" x2="2.4" y2="9.6" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" /></svg>
        </button>
      </div>

      <!-- body: nav + content -->
      <div class="flex flex-1 min-h-0">
        <!-- left nav -->
        <div class="w-[178px] shrink-0 border-r border-line p-3 flex flex-col gap-1" style="background: rgba(255,255,255,0.012);">
          {#each NAV as item (item.key)}
            {@const on = section === item.key}
            <button
              onclick={() => (section = item.key)}
              class="flex items-center gap-[10px] px-3 py-[9px] rounded-[9px] text-[13px] font-medium cursor-pointer transition-colors duration-150 text-left"
              style="color: {on ? 'var(--accent)' : 'var(--color-t-mid)'}; background: {on ? 'color-mix(in oklab, var(--accent) 13%, transparent)' : 'transparent'}; border: 1px solid {on ? 'color-mix(in oklab, var(--accent) 26%, transparent)' : 'transparent'};"
            >
              {item.label}
            </button>
          {/each}
          <div class="mt-auto font-mono text-[9px] text-t-lo leading-relaxed">
            bring-your-own-AI<br />no account · no telemetry
          </div>
        </div>

        <!-- content -->
        <div class="flex-1 min-w-0 overflow-y-auto px-6 py-5">
          {#if section === 'providers'}
            <h2 class="font-display text-[16px] font-semibold text-t-hi mb-1">AI providers</h2>
            <p class="text-[12.5px] text-t-mid mb-5">
              Sage runs on your own key and CLIs. Availability is re-checked every time the overlay
              opens.
            </p>

            <!-- Gemini -->
            <div class="rounded-[13px] border border-line p-4 mb-3" style="background: rgba(255,255,255,0.014);">
              <div class="flex items-center gap-[10px] mb-3">
                <span class="w-[9px] h-[9px] rounded-full" style="background: {PROVIDERS.gemini.dot}; box-shadow: 0 0 6px {PROVIDERS.gemini.dot};"></span>
                <div class="min-w-0">
                  <div class="text-[13.5px] font-semibold text-t-hi">Gemini</div>
                  <div class="font-mono text-[10.5px] text-t-lo">{PROVIDERS.gemini.model} · API</div>
                </div>
                {#if availability.gemini}
                  <span class="ml-auto pill ok">Ready</span>
                {:else}
                  <span class="ml-auto pill warn">Key needed</span>
                {/if}
              </div>
              <div class="text-[11.5px] text-t-mid mb-1.5">API key</div>
              <div class="relative">
                <input
                  type={revealKey ? 'text' : 'password'}
                  bind:value={geminiKey}
                  onblur={saveKey}
                  onkeydown={(e) => e.key === 'Enter' && saveKey()}
                  placeholder={availability.gemini ? '•••••••••••••• (stored — type to replace)' : 'Paste your Gemini API key'}
                  class="w-full pl-[13px] pr-[38px] py-[10px] rounded-[10px] border border-line text-t-hi font-mono text-[11.5px] outline-none transition-colors placeholder:text-t-lo focus:border-accent"
                  style="background: rgba(0,0,0,0.22);"
                />
                <button
                  onclick={() => (revealKey = !revealKey)}
                  aria-label={revealKey ? 'Hide key' : 'Reveal key'}
                  class="absolute right-2 top-1/2 -translate-y-1/2 w-[26px] h-[26px] grid place-items-center rounded-md text-t-lo hover:text-t-mid cursor-pointer"
                >
                  {#if revealKey}
                    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M17.9 17.9A10.4 10.4 0 0 1 12 20C5 20 1 12 1 12a19 19 0 0 1 5.1-6M9.9 4.2A10.4 10.4 0 0 1 12 4c7 0 11 8 11 8a19 19 0 0 1-2.2 3.2M1 1l22 22" /></svg>
                  {:else}
                    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle cx="12" cy="12" r="3" /></svg>
                  {/if}
                </button>
              </div>
              <div class="flex items-center justify-between mt-1.5">
                <span class="font-mono text-[9.5px] text-t-lo">stored locally · sent only to Google{keySaving ? ' · saving…' : ''}</span>
                <button onclick={() => openUrl(KEY_URL)} class="text-[11px] cursor-pointer" style="color: var(--accent);">Get a key ↗</button>
              </div>
            </div>

            <!-- Claude -->
            <div class="rounded-[13px] border border-line px-4 py-[13px] mb-3 flex items-center gap-[10px]" style="background: rgba(255,255,255,0.014);">
              <span class="w-[9px] h-[9px] rounded-full" style="background: {PROVIDERS.claude.dot}; box-shadow: 0 0 6px {PROVIDERS.claude.dot};"></span>
              <div class="min-w-0">
                <div class="text-[13.5px] font-semibold text-t-hi">Claude</div>
                <div class="font-mono text-[10.5px] text-t-lo">
                  {PROVIDERS.claude.model} · CLI{availability.claude_where ? ` · ${availability.claude_where}` : ''}
                </div>
              </div>
              <span class="ml-auto pill {availability.claude ? 'ok' : 'off'}">{availability.claude ? 'Detected' : 'Not found'}</span>
            </div>

            <!-- Codex -->
            <div class="rounded-[13px] border border-line px-4 py-[13px] mb-3 flex items-center gap-[10px]" style="background: rgba(255,255,255,0.014);">
              <span class="w-[9px] h-[9px] rounded-full" style="background: {PROVIDERS.openai.dot}; box-shadow: 0 0 6px {PROVIDERS.openai.dot};"></span>
              <div class="min-w-0">
                <div class="text-[13.5px] font-semibold text-t-hi">OpenAI · Codex</div>
                <div class="font-mono text-[10.5px] text-t-lo">
                  {PROVIDERS.openai.model} · CLI{availability.openai_where ? ` · ${availability.openai_where}` : ''} · no screenshots
                </div>
              </div>
              <span class="ml-auto pill {availability.openai ? 'ok' : 'off'}">{availability.openai ? 'Detected' : 'Not found'}</span>
            </div>

            <!-- CLI detail + recheck -->
            <div class="flex items-center justify-between mb-5">
              <span class="font-mono text-[9.5px] text-t-lo max-w-[60%]">CLIs detected on PATH, then inside WSL.</span>
              <button
                onclick={recheck}
                disabled={rechecking}
                class="flex items-center gap-[7px] px-[13px] py-[8px] rounded-[9px] border border-line text-[12px] text-t-mid cursor-pointer transition-colors hover:text-t-hi disabled:opacity-60"
                style="background: var(--color-ink-2);"
              >
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" style={rechecking ? 'animation: spin 0.9s linear infinite;' : ''}><path d="M3 12a9 9 0 1 0 3-6.7L3 8" /><path d="M3 3v5h5" /></svg>
                {rechecking ? 'Re-checking…' : 'Re-check CLIs'}
              </button>
            </div>

            <!-- default provider -->
            <div class="text-[12.5px] text-t-mid mb-2">Default provider <span class="text-t-lo">— used when the overlay opens</span></div>
            <div class="flex gap-[7px]">
              {#each ['gemini', 'claude', 'openai'] as p (p)}
                {@const key = p as Provider}
                {@const avail = availability[key]}
                {@const active = key === provider && avail}
                <button
                  onclick={() => pickProvider(key)}
                  disabled={!avail}
                  title={avail ? '' : 'Not available'}
                  class="flex-1 flex items-center justify-center gap-[7px] py-[10px] rounded-[9px] text-[12.5px] font-medium transition-colors duration-150"
                  class:cursor-pointer={avail}
                  class:cursor-not-allowed={!avail}
                  style="border: 1px solid {active ? 'color-mix(in oklab, var(--accent) 34%, transparent)' : 'var(--color-line)'}; background: {active ? 'color-mix(in oklab, var(--accent) 16%, transparent)' : 'rgba(255,255,255,0.02)'}; color: {active ? 'var(--color-t-hi)' : 'var(--color-t-mid)'}; opacity: {avail ? 1 : 0.4};"
                >
                  <span class="w-[7px] h-[7px] rounded-full" style="background: {PROVIDERS[key].dot}; box-shadow: 0 0 6px {PROVIDERS[key].dot};"></span>
                  {PROVIDERS[key].label}
                </button>
              {/each}
            </div>
          {:else if section === 'hotkeys'}
            <h2 class="font-display text-[16px] font-semibold text-t-hi mb-1">Global hotkeys</h2>
            <p class="text-[12.5px] text-t-mid mb-5">Work from inside any game while Sage runs in the background.</p>
            {#each HOTKEYS as h (h.title)}
              <div class="flex items-center py-[15px] border-b border-line-2">
                <div class="min-w-0">
                  <div class="text-[13.5px] font-semibold text-t-hi">{h.title}</div>
                  <div class="text-[12px] text-t-mid">{h.sub}</div>
                </div>
                <div class="ml-auto flex items-center gap-[6px]">
                  <span class="keycap">Ctrl</span><span class="text-t-lo text-[11px]">+</span>
                  <span class="keycap">Shift</span><span class="text-t-lo text-[11px]">+</span>
                  <span class="keycap accent">{h.keys}</span>
                </div>
              </div>
            {/each}
            <div class="mt-5 flex items-start gap-[9px] px-[14px] py-[11px] rounded-[11px] border" style="border-color: color-mix(in oklab, var(--color-warn) 28%, transparent); background: color-mix(in oklab, var(--color-warn) 8%, transparent);">
              <svg class="shrink-0 mt-[1px]" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="var(--color-warn)" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9" /><path d="M12 8v5M12 16.5v.01" /></svg>
              <span class="text-[12px] text-t-mid leading-relaxed">Chords are fixed in this build (they avoid <span class="font-mono text-[11px]">Ctrl+Alt</span> / AltGr conflicts). Rebinding lands in a later update.</span>
            </div>
          {:else if section === 'launcher'}
            <h2 class="font-display text-[16px] font-semibold text-t-hi mb-1">Launcher</h2>
            <p class="text-[12.5px] text-t-mid mb-5">How Sage behaves on your desktop.</p>
            {#each TOGGLES as t (t.key)}
              {@const on = settings[t.key] as boolean}
              <div class="flex items-center py-[15px] border-b border-line-2">
                <div class="min-w-0">
                  <div class="text-[13.5px] font-semibold text-t-hi">{t.label}</div>
                  <div class="text-[12px] text-t-mid">{t.sub}</div>
                </div>
                <button
                  role="switch"
                  aria-checked={on}
                  aria-label={t.label}
                  onclick={() => ((settings[t.key] as boolean) = !on)}
                  class="ml-auto relative w-[42px] h-[23px] rounded-full border-none cursor-pointer transition-colors duration-200 shrink-0"
                  style="background: {on ? 'var(--accent)' : 'rgba(255,255,255,0.13)'};"
                >
                  <span class="absolute top-[3px] w-[17px] h-[17px] rounded-full bg-white transition-all duration-200" style="left: {on ? '22px' : '3px'};"></span>
                </button>
              </div>
            {/each}
          {:else}
            <div class="flex items-center gap-[14px] mb-4">
              <span class="relative w-[46px] h-[46px] rounded-full shrink-0" style="background: radial-gradient(circle at 50% 38%, #fff 0%, color-mix(in oklab, var(--accent) 85%, white) 26%, var(--accent) 60%, transparent 100%); box-shadow: 0 0 26px -4px var(--accent);"></span>
              <div>
                <div class="font-display text-[20px] font-bold tracking-[0.06em] text-t-hi">SAGE</div>
                <div class="font-mono text-[10.5px] text-t-lo">{VERSION} · external overlay</div>
              </div>
            </div>
            <p class="text-[13px] text-t-mid leading-relaxed mb-4">
              A lightweight in-game AI companion. Sage runs as its own transparent window composited
              over your game — no injection, no account, no telemetry. Bring your own Gemini key or
              Claude / Codex CLI.
            </p>
            <div class="rounded-[11px] border border-line overflow-hidden mb-4">
              <div class="flex items-center justify-between px-[15px] py-[11px] border-b border-line-2">
                <span class="text-[12.5px] text-t-mid">Data folder</span>
                <span class="font-mono text-[10.5px] text-t-lo">%APPDATA%\com.aigamecompanion.launcher</span>
              </div>
              <div class="flex items-center justify-between px-[15px] py-[11px]">
                <span class="text-[12.5px] text-t-mid">Capture backend</span>
                <span class="font-mono text-[10.5px] text-t-lo">Windows.Graphics.Capture</span>
              </div>
            </div>
            <div class="flex gap-[10px]">
              <button onclick={openConfigFolder} class="flex-1 py-[11px] rounded-[10px] border border-line text-[12.5px] text-t-mid cursor-pointer transition-colors hover:text-t-hi" style="background: var(--color-ink-2);">Open config folder</button>
              <button onclick={openLogs} class="flex-1 py-[11px] rounded-[10px] border border-line text-[12.5px] text-t-mid cursor-pointer transition-colors hover:text-t-hi" style="background: var(--color-ink-2);">Open logs</button>
            </div>
          {/if}
        </div>
      </div>

      <!-- footer -->
      <div class="flex items-center px-[22px] py-[13px] border-t border-line">
        {#if saveError}
          <span class="text-[11.5px] mr-auto" style="color: var(--color-err);">{saveError}</span>
        {:else}
          <span class="font-mono text-[10px] text-t-lo mr-auto">changes apply immediately</span>
        {/if}
        <div class="flex gap-[10px]">
          <button onclick={() => (open = false)} class="px-4 py-2 rounded-[9px] text-[12.5px] font-medium text-t-mid cursor-pointer transition-colors hover:text-t-hi hover:bg-white/[0.05]">Cancel</button>
          <button onclick={save} disabled={saving} class="px-[18px] py-2 rounded-[9px] text-[12.5px] font-semibold cursor-pointer transition-all enabled:hover:brightness-110 disabled:opacity-60" style="background: var(--accent); color: #0b0b0d;">
            {saving ? 'Saving…' : 'Save changes'}
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    padding: 3px 10px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 500;
    white-space: nowrap;
  }
  .pill.ok {
    color: var(--color-ok);
    background: color-mix(in oklab, var(--color-ok) 14%, transparent);
    border: 1px solid color-mix(in oklab, var(--color-ok) 30%, transparent);
  }
  .pill.warn {
    color: var(--color-warn);
    background: color-mix(in oklab, var(--color-warn) 14%, transparent);
    border: 1px solid color-mix(in oklab, var(--color-warn) 30%, transparent);
  }
  .pill.off {
    color: var(--color-t-lo);
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid var(--color-line);
  }
  .keycap {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--color-t-hi);
    padding: 4px 9px;
    border-radius: 7px;
    background: var(--color-ink-3);
    border: 1px solid var(--color-line);
    box-shadow: 0 1.5px 0 rgba(0, 0, 0, 0.4);
  }
  .keycap.accent {
    color: var(--accent);
    border-color: color-mix(in oklab, var(--accent) 34%, transparent);
    background: color-mix(in oklab, var(--accent) 12%, transparent);
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
