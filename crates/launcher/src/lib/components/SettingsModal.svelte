<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { PROVIDERS, getProvider, setProvider, type Provider } from '../stores/companion.svelte';

  interface Settings {
    overlay_dll_path: string | null;
    scan_on_startup: boolean;
    minimize_to_tray: boolean;
    launch_on_startup: boolean;
  }

  let { open = $bindable(false) }: { open: boolean } = $props();

  let settings = $state<Settings>({
    overlay_dll_path: null,
    scan_on_startup: true,
    minimize_to_tray: true,
    launch_on_startup: false,
  });
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let provider = $derived(getProvider());

  const providerKeys = Object.keys(PROVIDERS) as Provider[];

  async function load() {
    try {
      settings = await invoke<Settings>('get_settings');
    } catch (e) {
      console.error('Failed to load settings:', e);
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

  $effect(() => {
    if (open) load();
  });

  function onBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) open = false;
  }
  function onKeydown(e: KeyboardEvent) {
    if (open && e.key === 'Escape') open = false;
  }

  const toggles: { key: keyof Settings; label: string }[] = [
    { key: 'scan_on_startup', label: 'Scan games on startup' },
    { key: 'minimize_to_tray', label: 'Minimize to tray' },
    { key: 'launch_on_startup', label: 'Launch on system startup' },
  ];
</script>

<!-- Window-level handler so Esc closes even before focus enters the dialog. -->
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
    style="background: rgba(6, 6, 8, 0.62); backdrop-filter: blur(6px); animation: fade-up 0.18s ease;"
  >
    <div
      class="w-[480px] max-w-[90%] rounded-2xl border border-line overflow-hidden"
      style="background: var(--color-ink-1); box-shadow: 0 30px 70px rgba(0,0,0,0.6); animation: fade-up 0.22s ease;"
    >
      <!-- header -->
      <div class="flex items-center justify-between px-[22px] py-[18px] border-b border-line">
        <span class="font-display text-[15px] font-semibold tracking-[0.04em] text-t-hi"
          >Settings</span
        >
        <button
          onclick={() => (open = false)}
          aria-label="Close settings"
          class="w-[30px] h-[30px] grid place-items-center rounded-lg text-t-mid cursor-pointer transition-all duration-150 hover:text-white"
          onmouseenter={(e) =>
            ((e.currentTarget as HTMLElement).style.background = 'rgba(232,72,72,0.7)')}
          onmouseleave={(e) => ((e.currentTarget as HTMLElement).style.background = 'transparent')}
        >
          <svg width="12" height="12" viewBox="0 0 12 12"
            ><line
              x1="2.4"
              y1="2.4"
              x2="9.6"
              y2="9.6"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
            /><line
              x1="9.6"
              y1="2.4"
              x2="2.4"
              y2="9.6"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
            /></svg
          >
        </button>
      </div>

      <!-- body -->
      <div class="px-[22px] py-5 flex flex-col gap-[18px]">
        <!-- dll path -->
        <div class="flex flex-col gap-2">
          <span class="text-[12.5px] text-t-mid">Overlay DLL path</span>
          <input
            type="text"
            placeholder="Auto-detect (next to injector.exe)"
            value={settings.overlay_dll_path ?? ''}
            oninput={(e) =>
              (settings.overlay_dll_path = (e.target as HTMLInputElement).value || null)}
            class="w-full px-[13px] py-[10px] rounded-[10px] border border-line text-t-hi font-mono text-[11.5px] outline-none transition-colors placeholder:text-t-lo"
            style="background: rgba(255,255,255,0.04);"
          />
        </div>

        <!-- default provider -->
        <div class="flex flex-col gap-[9px]">
          <span class="text-[12.5px] text-t-mid">Default AI provider</span>
          <div class="flex gap-[7px]">
            {#each providerKeys as key (key)}
              {@const active = key === provider}
              <button
                onclick={() => setProvider(key)}
                class="flex-1 flex items-center justify-center gap-[7px] py-[10px] rounded-[9px] font-display text-[12.5px] font-medium cursor-pointer transition-all duration-150"
                style="
                  border: 1px solid {active
                  ? 'color-mix(in oklab, var(--accent) 34%, transparent)'
                  : 'var(--color-line)'};
                  background: {active
                  ? 'color-mix(in oklab, var(--accent) 16%, transparent)'
                  : 'rgba(255,255,255,0.02)'};
                  color: {active ? 'var(--color-t-hi)' : 'var(--color-t-mid)'};
                "
              >
                <span
                  class="w-[7px] h-[7px] rounded-full"
                  style="background: {PROVIDERS[key]
                    .dot}; box-shadow: 0 0 6px color-mix(in oklab, {PROVIDERS[key]
                    .dot} 53%, transparent);"
                ></span>
                {PROVIDERS[key].label}
              </button>
            {/each}
          </div>
        </div>

        <!-- toggles -->
        {#each toggles as t (t.key)}
          {@const on = settings[t.key] as boolean}
          <div class="flex items-center justify-between">
            <span class="text-[13px] text-t-mid">{t.label}</span>
            <button
              role="switch"
              aria-checked={on}
              aria-label={t.label}
              onclick={() => ((settings[t.key] as boolean) = !on)}
              class="relative w-[42px] h-[23px] rounded-full border-none cursor-pointer transition-colors duration-200"
              style="background: {on ? 'var(--accent)' : 'rgba(255,255,255,0.13)'};"
            >
              <span
                class="absolute top-[3px] w-[17px] h-[17px] rounded-full bg-white transition-all duration-200"
                style="left: {on ? '22px' : '3px'};"
              ></span>
            </button>
          </div>
        {/each}
      </div>

      <!-- footer -->
      {#if saveError}
        <div class="px-[22px] py-2 text-sm text-err">{saveError}</div>
      {/if}
      <div class="flex items-center justify-between px-[22px] py-4 border-t border-line">
        <span class="font-mono text-[10px] text-t-lo">no telemetry · bring-your-own-AI</span>
        <div class="flex gap-[10px]">
          <button
            onclick={() => (open = false)}
            class="px-4 py-2 rounded-[9px] font-display text-[12.5px] font-medium text-t-mid cursor-pointer transition-all duration-150 hover:text-t-hi hover:bg-white/[0.05]"
            >Cancel</button
          >
          <button
            onclick={save}
            disabled={saving}
            class="px-[18px] py-2 rounded-[9px] font-display text-[12.5px] font-semibold cursor-pointer transition-all duration-150 enabled:hover:brightness-110"
            style="background: var(--accent); color: #0b0b0d;"
          >
            {saving ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}
