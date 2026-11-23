// Module hooks that let `node --test` load the app's components as the client
// build does: `.svelte` files and `.svelte.ts` rune modules are compiled with
// `dev: false`, an import without its `.ts` extension is found, and packages
// resolve to their browser entry (`svelte`'s server entry has no `mount`).
// Registered by `mount.ts`.
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes, type LoadHook, type ResolveHook } from 'node:module';
import { fileURLToPath } from 'node:url';
import { compile, compileModule } from 'svelte/compiler';

export const resolve: ResolveHook = async (specifier, context, nextResolve) => {
  const browser = { ...context, conditions: [...context.conditions, 'browser'] };
  try {
    return await nextResolve(specifier, browser);
  } catch (error) {
    if (!specifier.startsWith('.')) throw error;
    return nextResolve(`${specifier}.ts`, browser);
  }
};

export const load: LoadHook = async (url, context, nextLoad) => {
  if (!url.startsWith('file:') || !(url.endsWith('.svelte') || url.endsWith('.svelte.ts'))) {
    return nextLoad(url, context);
  }
  const filename = fileURLToPath(url);
  const source = await readFile(filename, 'utf8');
  const { js } = url.endsWith('.svelte')
    ? compile(source, { filename, dev: false, generate: 'client', css: 'external', runes: true })
    : compileModule(stripTypeScriptTypes(source), { filename, dev: false, generate: 'client' });
  return { format: 'module', source: js.code, shortCircuit: true };
};
