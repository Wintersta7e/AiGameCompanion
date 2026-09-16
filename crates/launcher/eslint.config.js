import { defineConfig, globalIgnores } from 'eslint/config';
import js from '@eslint/js';
import ts from 'typescript-eslint';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import svelteConfig from './svelte.config.js';

export default defineConfig([
  // Build output, deps and the Rust backend.
  globalIgnores(['dist/', 'node_modules/', 'src-tauri/']),
  {
    files: ['**/*.js', '**/*.ts', '**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
    extends: [
      js.configs.recommended,
      // Type-aware strictness; needs the TypeScript program wired below.
      ts.configs.strictTypeChecked,
      ts.configs.stylisticTypeChecked,
      svelte.configs.all,
      // Must stay last: turns off the formatting rules prettier owns.
      svelte.configs.prettier,
    ],
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: {
        // The config files at the package root are outside tsconfig.json's
        // program, so they are type-checked against tsconfig.node.json.
        projectService: {
          allowDefaultProject: ['*.js', '*.ts'],
          defaultProject: './tsconfig.node.json',
        },
        tsconfigRootDir: import.meta.dirname,
      },
    },
    linterOptions: {
      reportUnusedDisableDirectives: 'error',
    },
    rules: {
      // The UI is driven by CSS custom properties and per-item computed colors
      // (accent, provider dot, status). Tailwind classes cannot express a value
      // that is only known at runtime, so inline style stays.
      'svelte/no-inline-styles': 'off',
      // Its autofix rewrites `style="a: b"` into `style:a="b"`, which svelte2tsx
      // (and therefore `svelte-check`, a required CI check) parses as an
      // expression: one converted gradient produced 20 bogus type errors.
      'svelte/prefer-style-directive': 'off',
      // Reads the selectors inside `@keyframes` (`0%`, `50%`, `to`) as element
      // type selectors, so every animation block reports. Nothing to configure
      // around it.
      'svelte/consistent-selector-style': 'off',
      // A number in a template literal is unambiguous; the risk this rule
      // guards against is `${object}` and `${null}`, which stay errors.
      '@typescript-eslint/restrict-template-expressions': ['error', { allowNumber: true }],
      // Every <script> must be TypeScript.
      'svelte/block-lang': ['error', { script: ['ts'], style: [null, 'css'] }],
      // Rules that assume a component API this app does not use.
      'svelte/experimental-require-slot-types': 'off',
      'svelte/experimental-require-strict-events': 'off',
      // Tailwind utilities are never declared in a <style> block, so this rule
      // flags every class in the project.
      'svelte/no-unused-class-name': 'off',
      'no-console': ['error', { allow: ['warn', 'error'] }],
      eqeqeq: ['error', 'always'],
      'no-implicit-coercion': 'error',
      'no-var': 'error',
      'object-shorthand': 'error',
      'prefer-const': 'error',
    },
  },
  {
    files: ['**/*.svelte', '**/*.svelte.ts', '**/*.svelte.js'],
    languageOptions: {
      parserOptions: {
        parser: ts.parser,
        extraFileExtensions: ['.svelte'],
        svelteConfig,
      },
    },
    rules: {
      // svelte/prefer-const understands runes ($props/$derived stay `let`).
      'prefer-const': 'off',
      // Reads `let { open = $bindable(false) } = $props()` as a redundant
      // default and its autofix deletes the $bindable() call, silently turning
      // a two-way bound prop into a read-only one.
      '@typescript-eslint/no-useless-default-assignment': 'off',
    },
  },
  {
    // Config files run in Node, not in the browser.
    files: ['*.config.js', '*.config.ts'],
    languageOptions: {
      globals: { ...globals.node },
    },
  },
]);
