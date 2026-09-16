<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { PROVIDERS, getProvider, setProvider, type Provider } from '../stores/companion.svelte';

  interface Availability {
    gemini: boolean;
    claude: boolean;
    openai: boolean;
    claude_where: string;
    openai_where: string;
  }
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
    {
      key: 'scan_on_startup',
      label: 'Scan games on startup',
      sub: 'Refresh the library when Sage launches',
    },
    {
      key: 'minimize_to_tray',
      label: 'Minimize to tray',
      sub: 'Keep watching for games in the background',
    },
    {
      key: 'launch_on_startup',
      label: 'Launch on system startup',
      sub: 'Start Sage when Windows boots',
    },
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

  // The key field persists on blur, and unmounting the modal never fires blur --
  // so every close path flushes a pending key first, and stays open on failure
  // rather than dropping it silently. Escape reaches this twice (dialog +
  // window handler), hence the re-entry guard.
  let closing = false;
  async function closeModal() {
    if (closing) return;
    closing = true;
    try {
      if (geminiKey.trim()) {
        await saveKey();
        if (saveError) return;
      }
      open = false;
    } finally {
      closing = false;
    }
  }

  async function save() {
    saving = true;
    saveError = null;
    try {
      await invoke('update_settings', { settings });
      await closeModal();
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  function openUrl(url: string) {
    void invoke('open_url', { url }).catch((err: unknown) => {
      console.error('Failed to open the URL:', err);
    });
  }
  function openConfigFolder() {
    void invoke('open_config_folder').catch((err: unknown) => {
      console.error('Failed to open the config folder:', err);
    });
  }
  function openLogs() {
    void invoke('open_game_logs').catch((err: unknown) => {
      console.error('Failed to open the logs:', err);
    });
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
      void load();
    }
  });

  function onBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) void closeModal();
  }
  function onKeydown(e: KeyboardEvent) {
    if (open && e.key === 'Escape') void closeModal();
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div
    style="background: rgba(6, 6, 8, 0.62); backdrop-filter: blur(6px);"
    class="absolute inset-0 z-[60] flex items-center justify-center"
    aria-label="Settings"
    aria-modal="true"
    onclick={onBackdrop}
    onkeydown={onKeydown}
    role="dialog"
  >
    <div
      style="background: var(--color-ink-1); box-shadow: 0 30px 70px rgba(0,0,0,0.6);"
      class="w-[680px] max-w-[92%] h-[560px] max-h-[92%] rounded-2xl border border-line overflow-hidden flex flex-col"
    >
      <!-- header -->
      <div class="flex items-center gap-3 px-[22px] py-[15px] border-b border-line">
        <span
          style="background: radial-gradient(circle at 50% 38%, #fff 0%, color-mix(in oklab, var(--accent) 85%, white) 26%, var(--accent) 60%, transparent 100%); box-shadow: 0 0 14px -3px var(--accent);"
          class="relative w-[22px] h-[22px] rounded-full shrink-0"
        ></span>
        <span class="font-display text-[15px] font-semibold tracking-[0.04em] text-t-hi"
          >Settings</span
        >
        <span
          class="font-mono text-[9px] text-t-lo px-[6px] py-[2px] rounded border border-line leading-tight"
          >SAGE · {VERSION}</span
        >
        <button
          class="ml-auto w-[30px] h-[30px] grid place-items-center rounded-lg text-t-mid cursor-pointer transition-colors duration-150 hover:text-white hover:bg-white/[0.06]"
          aria-label="Close settings"
          onclick={() => void closeModal()}
          type="button"
        >
          <svg height="12" viewBox="0 0 12 12" width="12"
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

      <!-- body: nav + content -->
      <div class="flex flex-1 min-h-0">
        <!-- left nav -->
        <div
          style="background: rgba(255,255,255,0.012);"
          class="w-[178px] shrink-0 border-r border-line p-3 flex flex-col gap-1"
        >
          {#each NAV as item (item.key)}
            {const on = $derived(section === item.key)}
            <button
              style="color: {on ? 'var(--accent)' : 'var(--color-t-mid)'}; background: {on
                ? 'color-mix(in oklab, var(--accent) 13%, transparent)'
                : 'transparent'}; border: 1px solid {on
                ? 'color-mix(in oklab, var(--accent) 26%, transparent)'
                : 'transparent'};"
              class="flex items-center gap-[10px] px-3 py-[9px] rounded-[9px] text-[13px] font-medium cursor-pointer transition-colors duration-150 text-left"
              onclick={() => (section = item.key)}
              type="button"
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
            <div
              style="background: rgba(255,255,255,0.014);"
              class="rounded-[13px] border border-line p-4 mb-3"
            >
              <div class="flex items-center gap-[10px] mb-3">
                <span
                  style="background: {PROVIDERS.gemini.dot}; box-shadow: 0 0 6px {PROVIDERS.gemini
                    .dot};"
                  class="w-[9px] h-[9px] rounded-full"
                ></span>
                <div class="min-w-0">
                  <div class="text-[13.5px] font-semibold text-t-hi">Gemini</div>
                  <div class="font-mono text-[10.5px] text-t-lo">
                    {PROVIDERS.gemini.model} · API
                  </div>
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
                  style="background: rgba(0,0,0,0.22);"
                  class="w-full pl-[13px] pr-[38px] py-[10px] rounded-[10px] border border-line text-t-hi font-mono text-[11.5px] outline-none transition-colors placeholder:text-t-lo focus:border-accent"
                  onblur={saveKey}
                  onkeydown={(e) => e.key === 'Enter' && saveKey()}
                  placeholder={availability.gemini
                    ? '•••••••••••••• (stored — type to replace)'
                    : 'Paste your Gemini API key'}
                  type={revealKey ? 'text' : 'password'}
                  bind:value={geminiKey}
                />
                <button
                  class="absolute right-2 top-1/2 -translate-y-1/2 w-[26px] h-[26px] grid place-items-center rounded-md text-t-lo hover:text-t-mid cursor-pointer"
                  aria-label={revealKey ? 'Hide key' : 'Reveal key'}
                  onclick={() => (revealKey = !revealKey)}
                  type="button"
                >
                  {#if revealKey}
                    <svg
                      fill="none"
                      height="16"
                      stroke="currentColor"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                      stroke-width="1.6"
                      viewBox="0 0 24 24"
                      width="16"
                      ><path
                        d="M17.9 17.9A10.4 10.4 0 0 1 12 20C5 20 1 12 1 12a19 19 0 0 1 5.1-6M9.9 4.2A10.4 10.4 0 0 1 12 4c7 0 11 8 11 8a19 19 0 0 1-2.2 3.2M1 1l22 22"
                      /></svg
                    >
                  {:else}
                    <svg
                      fill="none"
                      height="16"
                      stroke="currentColor"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                      stroke-width="1.6"
                      viewBox="0 0 24 24"
                      width="16"
                      ><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" /><circle
                        cx="12"
                        cy="12"
                        r="3"
                      /></svg
                    >
                  {/if}
                </button>
              </div>
              <div class="flex items-center justify-between mt-1.5">
                <span class="font-mono text-[9.5px] text-t-lo"
                  >stored locally · sent only to Google{keySaving ? ' · saving…' : ''}</span
                >
                <button
                  style="color: var(--accent);"
                  class="text-[11px] cursor-pointer"
                  onclick={() => {
                    openUrl(KEY_URL);
                  }}
                  type="button">Get a key ↗</button
                >
              </div>
            </div>

            <!-- Claude -->
            <div
              style="background: rgba(255,255,255,0.014);"
              class="rounded-[13px] border border-line px-4 py-[13px] mb-3 flex items-center gap-[10px]"
            >
              <span
                style="background: {PROVIDERS.claude.dot}; box-shadow: 0 0 6px {PROVIDERS.claude
                  .dot};"
                class="w-[9px] h-[9px] rounded-full"
              ></span>
              <div class="min-w-0">
                <div class="text-[13.5px] font-semibold text-t-hi">Claude</div>
                <div class="font-mono text-[10.5px] text-t-lo">
                  {PROVIDERS.claude.model} · CLI{availability.claude_where
                    ? ` · ${availability.claude_where}`
                    : ''}
                </div>
              </div>
              <span class="ml-auto pill {availability.claude ? 'ok' : 'off'}"
                >{availability.claude ? 'Detected' : 'Not found'}</span
              >
            </div>

            <!-- Codex -->
            <div
              style="background: rgba(255,255,255,0.014);"
              class="rounded-[13px] border border-line px-4 py-[13px] mb-3 flex items-center gap-[10px]"
            >
              <span
                style="background: {PROVIDERS.openai.dot}; box-shadow: 0 0 6px {PROVIDERS.openai
                  .dot};"
                class="w-[9px] h-[9px] rounded-full"
              ></span>
              <div class="min-w-0">
                <div class="text-[13.5px] font-semibold text-t-hi">OpenAI · Codex</div>
                <div class="font-mono text-[10.5px] text-t-lo">
                  {PROVIDERS.openai.model} · CLI{availability.openai_where
                    ? ` · ${availability.openai_where}`
                    : ''} · no screenshots
                </div>
              </div>
              <span class="ml-auto pill {availability.openai ? 'ok' : 'off'}"
                >{availability.openai ? 'Detected' : 'Not found'}</span
              >
            </div>

            <!-- CLI detail + recheck -->
            <div class="flex items-center justify-between mb-5">
              <span class="font-mono text-[9.5px] text-t-lo max-w-[60%]"
                >CLIs detected on PATH, then inside WSL.</span
              >
              <button
                style="background: var(--color-ink-2);"
                class="flex items-center gap-[7px] px-[13px] py-[8px] rounded-[9px] border border-line text-[12px] text-t-mid cursor-pointer transition-colors hover:text-t-hi disabled:opacity-60"
                disabled={rechecking}
                onclick={recheck}
                type="button"
              >
                <svg
                  class:spin={rechecking}
                  fill="none"
                  height="13"
                  stroke="currentColor"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  stroke-width="1.8"
                  viewBox="0 0 24 24"
                  width="13"><path d="M3 12a9 9 0 1 0 3-6.7L3 8" /><path d="M3 3v5h5" /></svg
                >
                {rechecking ? 'Re-checking…' : 'Re-check CLIs'}
              </button>
            </div>

            <!-- default provider -->
            <div class="text-[12.5px] text-t-mid mb-2">
              Default provider <span class="text-t-lo">— used when the overlay opens</span>
            </div>
            <div class="flex gap-[7px]">
              {#each ['gemini', 'claude', 'openai'] as p (p)}
                {const key = $derived(p as Provider)}
                {const avail = $derived(availability[key])}
                {const active = $derived(key === provider && avail)}
                <button
                  style="border: 1px solid {active
                    ? 'color-mix(in oklab, var(--accent) 34%, transparent)'
                    : 'var(--color-line)'}; background: {active
                    ? 'color-mix(in oklab, var(--accent) 16%, transparent)'
                    : 'rgba(255,255,255,0.02)'}; color: {active
                    ? 'var(--color-t-hi)'
                    : 'var(--color-t-mid)'}; opacity: {avail ? 1 : 0.4};"
                  class="flex-1 flex items-center justify-center gap-[7px] py-[10px] rounded-[9px] text-[12.5px] font-medium transition-colors duration-150"
                  class:cursor-not-allowed={!avail}
                  class:cursor-pointer={avail}
                  disabled={!avail}
                  onclick={() => {
                    pickProvider(key);
                  }}
                  title={avail ? '' : 'Not available'}
                  type="button"
                >
                  <span
                    style="background: {PROVIDERS[key].dot}; box-shadow: 0 0 6px {PROVIDERS[key]
                      .dot};"
                    class="w-[7px] h-[7px] rounded-full"
                  ></span>
                  {PROVIDERS[key].label}
                </button>
              {/each}
            </div>
          {:else if section === 'hotkeys'}
            <h2 class="font-display text-[16px] font-semibold text-t-hi mb-1">Global hotkeys</h2>
            <p class="text-[12.5px] text-t-mid mb-5">
              Work from inside any game while Sage runs in the background.
            </p>
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
            <div
              style="border-color: color-mix(in oklab, var(--color-warn) 28%, transparent); background: color-mix(in oklab, var(--color-warn) 8%, transparent);"
              class="mt-5 flex items-start gap-[9px] px-[14px] py-[11px] rounded-[11px] border"
            >
              <svg
                class="shrink-0 mt-[1px]"
                fill="none"
                height="15"
                stroke="var(--color-warn)"
                stroke-linecap="round"
                stroke-linejoin="round"
                stroke-width="1.7"
                viewBox="0 0 24 24"
                width="15"><circle cx="12" cy="12" r="9" /><path d="M12 8v5M12 16.5v.01" /></svg
              >
              <span class="text-[12px] text-t-mid leading-relaxed"
                >Chords are fixed in this build (they avoid <span class="font-mono text-[11px]"
                  >Ctrl+Alt</span
                > / AltGr conflicts). Rebinding lands in a later update.</span
              >
            </div>
          {:else if section === 'launcher'}
            <h2 class="font-display text-[16px] font-semibold text-t-hi mb-1">Launcher</h2>
            <p class="text-[12.5px] text-t-mid mb-5">How Sage behaves on your desktop.</p>
            {#each TOGGLES as t (t.key)}
              {const on = $derived(settings[t.key] as boolean)}
              <div class="flex items-center py-[15px] border-b border-line-2">
                <div class="min-w-0">
                  <div class="text-[13.5px] font-semibold text-t-hi">{t.label}</div>
                  <div class="text-[12px] text-t-mid">{t.sub}</div>
                </div>
                <button
                  style="background: {on ? 'var(--accent)' : 'rgba(255,255,255,0.13)'};"
                  class="ml-auto relative w-[42px] h-[23px] rounded-full border-none cursor-pointer transition-colors duration-200 shrink-0"
                  aria-checked={on}
                  aria-label={t.label}
                  onclick={() => ((settings[t.key] as boolean) = !on)}
                  role="switch"
                  type="button"
                >
                  <span
                    style="left: {on ? '22px' : '3px'};"
                    class="absolute top-[3px] w-[17px] h-[17px] rounded-full bg-white transition-all duration-200"
                  ></span>
                </button>
              </div>
            {/each}
          {:else}
            <div class="flex items-center gap-[14px] mb-4">
              <span
                style="background: radial-gradient(circle at 50% 38%, #fff 0%, color-mix(in oklab, var(--accent) 85%, white) 26%, var(--accent) 60%, transparent 100%); box-shadow: 0 0 26px -4px var(--accent);"
                class="relative w-[46px] h-[46px] rounded-full shrink-0"
              ></span>
              <div>
                <div class="font-display text-[20px] font-bold tracking-[0.06em] text-t-hi">
                  SAGE
                </div>
                <div class="font-mono text-[10.5px] text-t-lo">{VERSION} · external overlay</div>
              </div>
            </div>
            <p class="text-[13px] text-t-mid leading-relaxed mb-4">
              A lightweight in-game AI companion. Sage runs as its own transparent window composited
              over your game — no injection, no account, no telemetry. Bring your own Gemini key or
              Claude / Codex CLI.
            </p>
            <div class="rounded-[11px] border border-line overflow-hidden mb-4">
              <div
                class="flex items-center justify-between px-[15px] py-[11px] border-b border-line-2"
              >
                <span class="text-[12.5px] text-t-mid">Data folder</span>
                <span class="font-mono text-[10.5px] text-t-lo"
                  >%APPDATA%\com.aigamecompanion.launcher</span
                >
              </div>
              <div class="flex items-center justify-between px-[15px] py-[11px]">
                <span class="text-[12.5px] text-t-mid">Capture backend</span>
                <span class="font-mono text-[10.5px] text-t-lo">Windows.Graphics.Capture</span>
              </div>
            </div>
            <div class="flex gap-[10px]">
              <button
                style="background: var(--color-ink-2);"
                class="flex-1 py-[11px] rounded-[10px] border border-line text-[12.5px] text-t-mid cursor-pointer transition-colors hover:text-t-hi"
                onclick={openConfigFolder}
                type="button">Open config folder</button
              >
              <button
                style="background: var(--color-ink-2);"
                class="flex-1 py-[11px] rounded-[10px] border border-line text-[12.5px] text-t-mid cursor-pointer transition-colors hover:text-t-hi"
                onclick={openLogs}
                type="button">Open logs</button
              >
            </div>
          {/if}
        </div>
      </div>

      <!-- footer -->
      <div class="flex items-center px-[22px] py-[13px] border-t border-line">
        {#if saveError}
          <span style="color: var(--color-err);" class="text-[11.5px] mr-auto">{saveError}</span>
        {:else}
          <span class="font-mono text-[10px] text-t-lo mr-auto">changes apply immediately</span>
        {/if}
        <div class="flex gap-[10px]">
          <button
            class="px-4 py-2 rounded-[9px] text-[12.5px] font-medium text-t-mid cursor-pointer transition-colors hover:text-t-hi hover:bg-white/[0.05]"
            onclick={() => void closeModal()}
            type="button">Cancel</button
          >
          <button
            style="background: var(--accent); color: #0b0b0d;"
            class="px-[18px] py-2 rounded-[9px] text-[12.5px] font-semibold cursor-pointer transition-all enabled:hover:brightness-110 disabled:opacity-60"
            disabled={saving}
            onclick={save}
            type="button"
          >
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
  .spin {
    animation: spin 0.9s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
