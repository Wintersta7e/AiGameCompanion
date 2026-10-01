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
      reportUnusedInlineConfigs: 'error',
    },
    rules: {
      // The UI is driven by CSS custom properties and per-item computed colors
      // (accent, provider dot, status). Tailwind classes cannot express a value
      // that is only known at runtime, so inline style stays.
      'svelte/no-inline-styles': 'off',
      // A style-only preference: it would rewrite every inline style into
      // style: directives and catches no defect.
      'svelte/prefer-style-directive': 'off',
      // Prefers id and element selectors; this project styles by class.
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
      // typescript-eslint rules its presets leave off. Where a core rule has
      // a TypeScript-aware twin, the core rule is off and the twin is on.
      '@typescript-eslint/switch-exhaustiveness-check': [
        'error',
        { considerDefaultExhaustiveForUnions: false, requireDefaultForNonUnion: true },
      ],
      '@typescript-eslint/require-array-sort-compare': 'error',
      '@typescript-eslint/strict-void-return': 'error',
      '@typescript-eslint/method-signature-style': 'error',
      '@typescript-eslint/no-import-type-side-effects': 'error',
      '@typescript-eslint/explicit-module-boundary-types': 'error',
      'no-shadow': 'off',
      '@typescript-eslint/no-shadow': 'error',
      'default-param-last': 'off',
      '@typescript-eslint/default-param-last': 'error',
      'no-loop-func': 'off',
      '@typescript-eslint/no-loop-func': 'error',
      '@typescript-eslint/strict-boolean-expressions': [
        'error',
        { allowNumber: false, allowNullableBoolean: true, allowNullableString: true },
      ],
      // Core rules that catch a defect.
      'array-callback-return': 'error',
      'no-await-in-loop': 'error',
      'no-promise-executor-return': 'error',
      'no-self-compare': 'error',
      'no-template-curly-in-string': 'error',
      'no-unmodified-loop-condition': 'error',
      'no-unreachable-loop': 'error',
      'guard-for-in': 'error',
      'no-return-assign': 'error',
      'no-sequences': 'error',
      radix: 'error',
      'default-case-last': 'error',
      'no-extend-native': 'error',
      'no-alert': 'error',
      // Size and nesting caps.
      complexity: ['error', 20],
      'max-depth': ['error', 4],
      'max-nested-callbacks': ['error', 10],
      'max-lines-per-function': ['error', { max: 100, skipBlankLines: true, skipComments: true }],
      // Code that does nothing.
      'no-useless-call': 'error',
      'no-useless-computed-key': 'error',
      'no-useless-concat': 'error',
      'no-useless-rename': 'error',
      'no-useless-return': 'error',
      'no-lone-blocks': 'error',
      'no-extra-bind': 'error',
      'no-unneeded-ternary': 'error',
      'no-undef-init': 'error',
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
      // default; its autofix deletes the $bindable() call and turns a two-way
      // bound prop read-only (svelte-check then errors where a parent binds).
      '@typescript-eslint/no-useless-default-assignment': 'off',
    },
  },
  {
    // Package-root files run in Node, not in the browser.
    files: ['*.js', '*.ts'],
    languageOptions: {
      globals: { ...globals.node },
    },
  },
  {
    // Gate helpers and unit tests run in Node; typescript-eslint caps its
    // default project at 8 files, so they use the Node program directly.
    files: ['scripts/*.ts', 'src/**/*.test.ts'],
    languageOptions: {
      globals: { ...globals.node },
      parserOptions: { projectService: false, project: './tsconfig.node.json' },
    },
  },
]);
