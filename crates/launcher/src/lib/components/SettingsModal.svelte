<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

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

  async function load(): Promise<void> {
    try {
      settings = await invoke<Settings>("get_settings");
    } catch (e) {
      console.error("Failed to load settings:", e);
    }
  }

  async function save(): Promise<void> {
    saving = true;
    try {
      await invoke("update_settings", { settings });
      open = false;
    } catch (e) {
      console.error("Failed to save settings:", e);
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    if (open) {
      load();
    }
  });

  function onBackdropClick(e: MouseEvent): void {
    if (e.target === e.currentTarget) {
      open = false;
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      open = false;
    }
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div
    role="dialog"
    aria-modal="true"
    aria-label="Settings"
    class="fixed inset-0 z-50 flex items-center justify-center"
    onclick={onBackdropClick}
    onkeydown={onKeydown}
    style="background: rgba(0, 0, 0, 0.6); backdrop-filter: blur(4px);"
  >
    <div
      class="w-[480px] rounded-xl border border-border-subtle overflow-hidden animate-fade-up"
      style="background: var(--color-bg-surface); box-shadow: 0 24px 48px rgba(0, 0, 0, 0.4);"
    >
      <!-- Header -->
      <div class="flex items-center justify-between px-6 py-4 border-b border-border-subtle">
        <h2 class="font-display text-lg font-semibold text-text-primary tracking-wide uppercase">Settings</h2>
        <button
          class="w-8 h-8 flex items-center justify-center rounded-md text-text-secondary hover:text-white hover:bg-[rgba(255,60,60,0.7)] transition-all duration-150"
          onclick={() => (open = false)}
          aria-label="Close settings"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
            <line x1="2" y1="2" x2="10" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
            <line x1="10" y1="2" x2="2" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <!-- Body -->
      <div class="px-6 py-5 flex flex-col gap-4">
        <!-- Overlay DLL Path -->
        <div class="flex flex-col gap-1.5">
          <label for="dll-path" class="font-body text-sm text-text-secondary">Overlay DLL Path</label>
          <input
            id="dll-path"
            type="text"
            placeholder="Auto-detect (default)"
            value={settings.overlay_dll_path ?? ""}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).value;
              settings.overlay_dll_path = v || null;
            }}
            class="w-full px-3 py-2 rounded-lg border border-border-subtle bg-[rgba(255,255,255,0.04)] text-text-primary font-body text-sm outline-none focus:border-border-glow transition-colors placeholder:text-text-muted"
          />
        </div>

        <!-- Toggle: Scan on Startup -->
        <label class="flex items-center justify-between cursor-pointer group">
          <span class="font-body text-sm text-text-secondary group-hover:text-text-primary transition-colors">Scan games on startup</span>
          <button
            role="switch"
            aria-label="Scan games on startup"
            aria-checked={settings.scan_on_startup}
            onclick={() => (settings.scan_on_startup = !settings.scan_on_startup)}
            class="w-10 h-[22px] rounded-full relative transition-colors duration-200"
            style="background: {settings.scan_on_startup ? 'var(--color-accent)' : 'rgba(255,255,255,0.1)'};"
          >
            <span
              class="absolute top-[3px] w-4 h-4 rounded-full bg-white transition-transform duration-200"
              style="left: {settings.scan_on_startup ? '21px' : '3px'};"
            ></span>
          </button>
        </label>

        <!-- Toggle: Minimize to Tray -->
        <label class="flex items-center justify-between cursor-pointer group">
          <span class="font-body text-sm text-text-secondary group-hover:text-text-primary transition-colors">Minimize to tray</span>
          <button
            role="switch"
            aria-label="Minimize to tray"
            aria-checked={settings.minimize_to_tray}
            onclick={() => (settings.minimize_to_tray = !settings.minimize_to_tray)}
            class="w-10 h-[22px] rounded-full relative transition-colors duration-200"
            style="background: {settings.minimize_to_tray ? 'var(--color-accent)' : 'rgba(255,255,255,0.1)'};"
          >
            <span
              class="absolute top-[3px] w-4 h-4 rounded-full bg-white transition-transform duration-200"
              style="left: {settings.minimize_to_tray ? '21px' : '3px'};"
            ></span>
          </button>
        </label>

        <!-- Toggle: Launch on Startup -->
        <label class="flex items-center justify-between cursor-pointer group">
          <span class="font-body text-sm text-text-secondary group-hover:text-text-primary transition-colors">Launch on system startup</span>
          <button
            role="switch"
            aria-label="Launch on system startup"
            aria-checked={settings.launch_on_startup}
            onclick={() => (settings.launch_on_startup = !settings.launch_on_startup)}
            class="w-10 h-[22px] rounded-full relative transition-colors duration-200"
            style="background: {settings.launch_on_startup ? 'var(--color-accent)' : 'rgba(255,255,255,0.1)'};"
          >
            <span
              class="absolute top-[3px] w-4 h-4 rounded-full bg-white transition-transform duration-200"
              style="left: {settings.launch_on_startup ? '21px' : '3px'};"
            ></span>
          </button>
        </label>
      </div>

      <!-- Footer -->
      <div class="flex justify-end gap-3 px-6 py-4 border-t border-border-subtle">
        <button
          class="px-4 py-2 rounded-lg font-body text-sm text-text-secondary hover:text-text-primary hover:bg-[rgba(255,255,255,0.06)] transition-all duration-150"
          onclick={() => (open = false)}
        >
          Cancel
        </button>
        <button
          class="px-4 py-2 rounded-lg font-body text-sm text-white transition-all duration-150"
          style="background: linear-gradient(135deg, #638cff 0%, #a855f7 100%); box-shadow: 0 0 12px rgba(99, 140, 255, 0.25);"
          onclick={save}
          disabled={saving}
        >
          {saving ? "Saving..." : "Save"}
        </button>
      </div>
    </div>
  </div>
{/if}
