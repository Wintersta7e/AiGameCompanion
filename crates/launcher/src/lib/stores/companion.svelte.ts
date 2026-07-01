/**
 * Companion (AI provider) selection for the launcher UI. Mirrors the overlay's
 * persisted `active_provider`; `setProvider` persists via `set_active_provider`.
 */

import { invoke } from '@tauri-apps/api/core';

export type Provider = 'gemini' | 'claude' | 'openai';

export interface ProviderMeta {
  label: string;
  model: string;
  dot: string;
}

export const PROVIDERS: Record<Provider, ProviderMeta> = {
  gemini: { label: 'Gemini', model: 'gemini-2.5-flash', dot: '#5b9bff' },
  claude: { label: 'Claude', model: 'claude-sonnet-4.5', dot: '#d97757' },
  openai: { label: 'OpenAI', model: 'gpt-5-codex', dot: '#10a37f' },
};

let provider = $state<Provider>('gemini');

export function getProvider(): Provider {
  return provider;
}

export function getProviderMeta(): ProviderMeta {
  return PROVIDERS[provider];
}

export function setProvider(p: Provider): void {
  provider = p;
  void invoke('set_active_provider', { provider: p }).catch(() => {
    /* selection still applies for this session */
  });
}

/** Load the persisted provider on startup. */
export async function loadProvider(): Promise<void> {
  try {
    const settings = await invoke<{ active_provider?: string }>('get_settings');
    const saved = settings.active_provider;
    if (saved && saved in PROVIDERS) provider = saved as Provider;
  } catch {
    /* keep the default */
  }
}
