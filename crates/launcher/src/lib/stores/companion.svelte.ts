/**
 * Companion (AI provider) selection for the launcher UI. In the real app this
 * mirrors `[api].provider` in config.toml — wire setProvider() to a Tauri
 * command that persists the choice (and the overlay picks it up on next launch).
 */

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

const ORDER: Provider[] = ['gemini', 'claude', 'openai'];

let provider = $state<Provider>('gemini');

export function getProvider(): Provider {
  return provider;
}

export function getProviderMeta(): ProviderMeta {
  return PROVIDERS[provider];
}

export function setProvider(p: Provider): void {
  provider = p;
  // TODO: invoke('set_provider', { provider: p }) to persist into config.toml
}

export function cycleProvider(): void {
  setProvider(ORDER[(ORDER.indexOf(provider) + 1) % ORDER.length]);
}
