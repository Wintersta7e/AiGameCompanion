// Self-test of the pin guard: a literal clean tree that passes, then one
// literal change per violation shape, each of which must be reported under its
// rule and no other. A rule that scans nothing counts as reported.
//
//   node crates/launcher/scripts/pin-guard.selftest.ts
import { checkPins } from './pin-guard.ts';
import type { Rule } from './pin-guard.ts';
import type { ToolSource } from './freshness.ts';

const SHA_A = '1111111111111111111111111111111111111111111111111111111111111111';
const SHA_B = '2222222222222222222222222222222222222222222222222222222222222222';

const CI = [
  'name: CI',
  '',
  'on:',
  '  push:',
  '',
  '# Actions are pinned to commit SHAs.',
  'jobs:',
  '  build:',
  '    runs-on: ubuntu-24.04',
  '    steps:',
  '      - name: Check out',
  '        uses: actions/checkout@1111111111111111111111111111111111111111 # v7.0.1',
  '      - name: Set up Node',
  '        uses: actions/setup-node@2222222222222222222222222222222222222222 # v7.0.0',
  '        with:',
  '          node-version-file: .nvmrc',
  '      - name: Read the tool pins',
  '        id: pins',
  `        run: grep -E '^[A-Z][A-Z0-9_]*=' scripts/ci-tools.env >> "$GITHUB_OUTPUT"`,
  '      - name: Install the Rust toolchain',
  '        run: rustup toolchain install --no-self-update',
  '      - name: Install cargo-deny',
  '        uses: taiki-e/install-action@3333333333333333333333333333333333333333 # v2.87.13',
  '        with:',
  `          tool: cargo-deny@\${{ steps.pins.outputs.CARGO_DENY }}`,
  '          fallback: none',
  '      - name: Install gitleaks',
  '        env:',
  `          VERSION: \${{ steps.pins.outputs.GITLEAKS }}`,
  '        run: echo "$VERSION"',
  '  canary-one:',
  '    runs-on: windows-2025-vs2026',
  '    env:',
  '      RUSTUP_TOOLCHAIN: beta',
  '    steps:',
  '      - name: Install the beta toolchain',
  '        run: rustup toolchain install beta --profile minimal --component rustfmt,clippy --no-self-update',
  '  canary-two:',
  '    runs-on: ubuntu-24.04',
  '    env:',
  '      RUSTUP_TOOLCHAIN: beta',
  '    steps:',
  '      - name: Run the rows',
  '        run: ./scripts/ci-check.sh --job rust',
  '',
].join('\n');

const CLEAN: Readonly<Record<string, string>> = {
  '.github/workflows/ci.yml': CI,
  'scripts/ci-check.sh': [
    '#!/usr/bin/env bash',
    '# Reads the override, never sets it.',
    `if [[ -n "\${RUSTUP_TOOLCHAIN:-}" ]]; then echo 'in the repository, run rustup toolchain install'; fi`,
    'cargo clippy --workspace --locked -- -D warnings',
    '',
  ].join('\n'),
  'scripts/build.sh': '#!/usr/bin/env bash\ncargo xwin build --release --locked\n',
  'rust-toolchain.toml': [
    '[toolchain]',
    'channel = "1.90.0"',
    'components = ["rustfmt", "clippy"]',
    'targets = ["x86_64-pc-windows-msvc"]',
    'profile = "minimal"',
    '',
  ].join('\n'),
  '.nvmrc': '22.20.0\n',
  'scripts/ci-tools.env': [
    '# Tool versions for the gates.',
    '',
    'CARGO_DENY=0.20.2',
    'GITLEAKS=8.30.1',
    `GITLEAKS_SHA256=${SHA_A}`,
    'ACTIONLINT=1.7.12',
    `ACTIONLINT_SHA256=${SHA_B}`,
    'XWIN_SDK_VERSION=10.0.26100',
    'XWIN_CRT_VERSION=14.44.17.14',
    '',
  ].join('\n'),
  'crates/launcher/package.json':
    '{\n  "devDependencies": {\n    "@types/node": "^22.1.0"\n  }\n}\n',
  '.github/dependabot.yml': [
    'version: 2',
    'updates:',
    "  - package-ecosystem: 'cargo'",
    "    directory: '/'",
    '    schedule:',
    "      interval: 'weekly'",
    '    cooldown:',
    '      default-days: 7',
    '    groups:',
    '      cargo-minor-patch:',
    "        patterns: ['*']",
    "        update-types: ['minor', 'patch']",
    '    ignore:',
    '      # Held on purpose: its next major is a different library.',
    "      - dependency-name: 'held-crate'",
    "        update-types: ['version-update:semver-major']",
    '',
    '  # Commit pins only stay current if something updates them.',
    "  - package-ecosystem: 'github-actions'",
    "    directory: '/'",
    '    schedule:',
    "      interval: 'weekly'",
    '    cooldown:',
    '      default-days: 7',
    '    groups:',
    '      actions-minor-patch:',
    "        patterns: ['*']",
    "        update-types: ['minor', 'patch']",
    '',
    "  - package-ecosystem: 'npm'",
    "    directory: '/crates/launcher'",
    '    schedule:',
    "      interval: 'weekly'",
    '    cooldown:',
    '      default-days: 7',
    '    groups:',
    '      npm-minor-patch:',
    "        patterns: ['*']",
    "        update-types: ['minor', 'patch']",
    '',
    "  - package-ecosystem: 'rust-toolchain'",
    "    directory: '/'",
    '    schedule:',
    "      interval: 'weekly'",
    '    cooldown:',
    '      default-days: 7',
    '',
  ].join('\n'),
};

const TOOL_SOURCES: readonly ToolSource[] = [
  { key: 'CARGO_DENY', repo: 'example-org/deny-tool', manifest: 'cargo-deny' },
  { key: 'GITLEAKS', repo: 'example-org/leak-tool', manifest: null },
  { key: 'ACTIONLINT', repo: 'example-org/lint-tool', manifest: null },
];
const RUNNER_TOOLS: readonly string[] = [
  'node',
  'npm:vite',
  'rust',
  'cargo-deny',
  'gitleaks',
  'jq',
];

interface Case {
  readonly name: string;
  // Files that replace the clean tree's; null removes one.
  readonly files?: Readonly<Record<string, string | null>>;
  readonly unreadable?: Readonly<Record<string, string>>;
  readonly toolSources?: readonly ToolSource[];
  readonly runnerTools?: readonly string[] | null;
  readonly expect: readonly Rule[];
}

// Replaces `from` (which must occur exactly once) in one clean file.
const edit = (file: string, from: string, to: string): Readonly<Record<string, string>> => {
  const text = CLEAN[file] ?? '';
  const found = text.split(from).length - 1;
  if (found !== 1)
    throw new Error(`the literal "${from}" occurs ${String(found)} times in ${file}`);
  return { [file]: text.replace(from, to) };
};

const WF = '.github/workflows/ci.yml';
const NODE_STEP = '      - name: Set up Node\n';
// Adds one step to the build job, before the Node step.
const step = (lines: string): Readonly<Record<string, string>> =>
  edit(WF, NODE_STEP, `${lines}${NODE_STEP}`);
const CHECKOUT =
  '        uses: actions/checkout@1111111111111111111111111111111111111111 # v7.0.1\n';
const BUILD_JOB = '  build:\n    runs-on: ubuntu-24.04\n';
const TWO = '  canary-two:\n    runs-on: ubuntu-24.04\n    env:\n      RUSTUP_TOOLCHAIN: beta\n';
const INSTALL = `          tool: cargo-deny@\${{ steps.pins.outputs.CARGO_DENY }}\n`;

const TOOLCHAIN: Rule = 'toolchain names';
const NODE: Rule = 'Node version source';
const RUNNERS: Rule = 'runner labels';
const ACTIONS: Rule = 'action pins';
const ENTRIES: Rule = 'install-action entries';
const LITERALS: Rule = 'version literals';
const TYPES: Rule = '@types/node major';
const SHAPES: Rule = 'pin file shapes';

const toolchainCases: readonly Case[] = [
  { name: 'the clean tree passes', expect: [] },
  {
    name: 'the rust-toolchain action',
    files: step(
      '      - name: Toolchain\n        uses: dtolnay/rust-toolchain@4444444444444444444444444444444444444444 # v1.0.0\n',
    ),
    expect: [TOOLCHAIN],
  },
  {
    name: 'cargo with a toolchain name',
    files: step('      - name: Format\n        run: cargo +nightly fmt\n'),
    expect: [TOOLCHAIN],
  },
  {
    name: 'rustup default',
    files: step('      - name: Default\n        run: rustup default stable\n'),
    expect: [TOOLCHAIN],
  },
  {
    name: 'rustup override',
    files: step('      - name: Override\n        run: rustup override set stable\n'),
    expect: [TOOLCHAIN],
  },
  {
    name: 'rustup installing another toolchain',
    files: step('      - name: Nightly\n        run: rustup toolchain install nightly\n'),
    expect: [TOOLCHAIN],
  },
  {
    name: 'rustup installing another toolchain after its options',
    files: step(
      '      - name: Nightly\n        run: rustup toolchain install --profile minimal nightly\n',
    ),
    expect: [TOOLCHAIN],
  },
  {
    name: 'a third job with another toolchain override',
    files: edit(WF, BUILD_JOB, `${BUILD_JOB}    env:\n      RUSTUP_TOOLCHAIN: nightly\n`),
    expect: [TOOLCHAIN],
  },
  {
    name: 'one beta override removed',
    files: edit(WF, TWO, '  canary-two:\n    runs-on: ubuntu-24.04\n'),
    expect: [TOOLCHAIN],
  },
  {
    name: 'a third beta override',
    files: edit(WF, BUILD_JOB, `${BUILD_JOB}    env:\n      RUSTUP_TOOLCHAIN: beta\n`),
    expect: [TOOLCHAIN],
  },
  {
    name: 'the runner exporting the override',
    files: edit(
      'scripts/ci-check.sh',
      'cargo clippy',
      'export RUSTUP_TOOLCHAIN=beta\ncargo clippy',
    ),
    expect: [TOOLCHAIN],
  },
  {
    name: 'the build script setting the override',
    files: edit(
      'scripts/build.sh',
      'cargo xwin build',
      'RUSTUP_TOOLCHAIN=beta cargo clippy\ncargo xwin build',
    ),
    expect: [TOOLCHAIN],
  },
];

const workflowCases: readonly Case[] = [
  {
    name: 'a node-version input',
    files: edit(WF, 'node-version-file: .nvmrc', 'node-version: 22'),
    expect: [NODE],
  },
  {
    name: 'setup-node reading another file',
    files: edit(WF, 'node-version-file: .nvmrc', 'node-version-file: .node-version'),
    expect: [NODE],
  },
  {
    name: 'a moving runner label',
    files: edit(
      WF,
      '  canary-two:\n    runs-on: ubuntu-24.04',
      '  canary-two:\n    runs-on: ubuntu-latest',
    ),
    expect: [RUNNERS],
  },
  {
    name: 'a runner label from an expression',
    files: edit(WF, BUILD_JOB, `  build:\n    runs-on: \${{ matrix.os }}\n`),
    expect: [RUNNERS],
  },
  {
    name: 'a runner label without the Visual Studio version',
    files: edit(WF, 'runs-on: windows-2025-vs2026', 'runs-on: windows-2025'),
    expect: [RUNNERS],
  },
  {
    name: 'an action pinned to a tag',
    files: edit(WF, CHECKOUT, '        uses: actions/checkout@v7\n'),
    expect: [ACTIONS],
  },
  {
    name: 'a commit pin with a major-only comment',
    files: edit(
      WF,
      CHECKOUT,
      '        uses: actions/checkout@1111111111111111111111111111111111111111 # v7\n',
    ),
    expect: [ACTIONS],
  },
  {
    name: 'a commit pin without a comment',
    files: edit(
      WF,
      CHECKOUT,
      '        uses: actions/checkout@1111111111111111111111111111111111111111\n',
    ),
    expect: [ACTIONS],
  },
  {
    name: 'an upper-case commit pin',
    files: edit(
      WF,
      CHECKOUT,
      '        uses: actions/checkout@AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA # v7.0.1\n',
    ),
    expect: [ACTIONS],
  },
  {
    name: 'a local action',
    files: step('      - name: Local\n        uses: ./.github/actions/local\n'),
    expect: [ACTIONS],
  },
  {
    name: 'an install-action tool without a version',
    files: edit(WF, INSTALL, '          tool: cargo-deny\n'),
    expect: [ENTRIES],
  },
  {
    name: 'an install-action tool at latest',
    files: edit(WF, INSTALL, '          tool: cargo-deny@latest\n'),
    expect: [ENTRIES],
  },
  {
    name: 'an install-action step that may fall back',
    files: edit(WF, '          fallback: none\n', ''),
    expect: [ENTRIES],
  },
  {
    name: 'a version literal in a workflow',
    files: edit(WF, `VERSION: \${{ steps.pins.outputs.GITLEAKS }}`, 'VERSION: 1.7.12'),
    expect: [LITERALS],
  },
  {
    name: 'a checksum literal in a workflow',
    files: edit(
      WF,
      '        run: echo "$VERSION"\n',
      '        run: echo "$VERSION 0000000000000000000000000000000000000000000000000000000000000000"\n',
    ),
    expect: [LITERALS],
  },
  {
    name: 'a version literal in a runner comment',
    files: edit('scripts/ci-check.sh', 'cargo clippy', '# needs cargo-deny 0.20.2\ncargo clippy'),
    expect: [LITERALS],
  },
  {
    name: 'a workflow directory whose only file has no uses line',
    files: {
      [WF]: [
        'name: CI',
        'on:',
        '  push:',
        'jobs:',
        '  canary-one:',
        '    runs-on: ubuntu-24.04',
        '    env:',
        '      RUSTUP_TOOLCHAIN: beta',
        '    steps:',
        '      - name: One',
        '        run: echo one',
        '  canary-two:',
        '    runs-on: windows-2025-vs2026',
        '    env:',
        '      RUSTUP_TOOLCHAIN: beta',
        '    steps:',
        '      - name: Two',
        '        run: echo two',
        '',
      ].join('\n'),
    },
    expect: [ACTIONS],
  },
];

const pinFileCases: readonly Case[] = [
  {
    name: '@types/node a major ahead of Node',
    files: edit('crates/launcher/package.json', '"^22.1.0"', '"^26.6.2"'),
    expect: [TYPES],
  },
  {
    name: 'a fifth toolchain key',
    files: edit(
      'rust-toolchain.toml',
      'profile = "minimal"\n',
      'profile = "minimal"\npath = "x"\n',
    ),
    expect: [SHAPES],
  },
  {
    name: 'no toolchain profile',
    files: edit('rust-toolchain.toml', 'profile = "minimal"\n', ''),
    expect: [SHAPES],
  },
  {
    name: 'a channel name instead of a version',
    files: edit('rust-toolchain.toml', 'channel = "1.90.0"', 'channel = "stable"'),
    expect: [SHAPES],
  },
  {
    name: 'no toolchain file',
    files: { 'rust-toolchain.toml': null },
    expect: [SHAPES],
  },
  { name: 'a v before the Node version', files: { '.nvmrc': 'v22.20.0\n' }, expect: [SHAPES] },
  { name: 'two Node versions', files: { '.nvmrc': '22.20.0\n22.21.0\n' }, expect: [SHAPES] },
  {
    name: 'an unreadable Node file',
    files: { '.nvmrc': null },
    unreadable: { '.nvmrc': 'EACCES' },
    expect: [SHAPES],
  },
  {
    name: 'a tool line with spaces around =',
    files: edit('scripts/ci-tools.env', 'CARGO_DENY=0.20.2', 'CARGO_DENY = 0.20.2'),
    expect: [SHAPES],
  },
  {
    name: 'a key twice',
    files: edit('scripts/ci-tools.env', 'GITLEAKS=8.30.1\n', 'GITLEAKS=8.30.1\nGITLEAKS=8.30.0\n'),
    expect: [SHAPES],
  },
  {
    name: 'a checksum one digit short',
    files: edit(
      'scripts/ci-tools.env',
      `GITLEAKS_SHA256=${SHA_A}`,
      `GITLEAKS_SHA256=${SHA_A.slice(1)}`,
    ),
    expect: [SHAPES],
  },
  {
    name: 'a missing tool key',
    files: edit('scripts/ci-tools.env', 'ACTIONLINT=1.7.12\n', ''),
    expect: [SHAPES],
  },
  {
    name: 'a key the freshness report does not know',
    files: edit('scripts/ci-tools.env', 'XWIN_SDK_VERSION', 'EXTRA_TOOL=1\nXWIN_SDK_VERSION'),
    expect: [SHAPES],
  },
  {
    name: 'a runner tool whose key the tool file lacks',
    toolSources: [
      ...TOOL_SOURCES,
      { key: 'CARGO_EXTRA', repo: 'example-org/extra-tool', manifest: 'cargo-extra' },
    ],
    runnerTools: [...RUNNER_TOOLS, 'cargo-extra'],
    expect: [SHAPES],
  },
  { name: 'an unreadable runner table', runnerTools: null, expect: [SHAPES] },
];

const runCase = (c: Case): string | null => {
  const files = new Map<string, string>();
  for (const [file, text] of Object.entries({ ...CLEAN, ...c.files })) {
    if (text !== null) files.set(file, text);
  }
  const result = checkPins({
    files,
    unreadable: new Map(Object.entries(c.unreadable ?? {})),
    toolSources: c.toolSources ?? TOOL_SOURCES,
    runnerTools: c.runnerTools === undefined ? RUNNER_TOOLS : c.runnerTools,
  });
  const reported = new Set<string>([
    ...result.violations.map((violation) => violation.rule),
    ...result.scans.filter((scan) => scan.scanned === 0).map((scan) => scan.rule),
  ]);
  const got = [...reported].sort((a, b) => a.localeCompare(b));
  const want = [...new Set(c.expect)].sort((a, b) => a.localeCompare(b));
  if (got.join('|') === want.join('|')) return null;
  const detail = result.violations.map((v) => `${v.file}:${String(v.line)}: ${v.rule}: ${v.text}`);
  return `${c.name}: reported [${got.join(', ')}], expected [${want.join(', ')}] (${detail.join('; ')})`;
};

const DEPENDABOT = '.github/dependabot.yml';
const POLICY: Rule = 'Dependabot policy';
const TOOLCHAIN_ENTRY =
  "  - package-ecosystem: 'rust-toolchain'\n    directory: '/'\n    schedule:\n      interval: 'weekly'\n    cooldown:\n      default-days: 7\n";
const NPM_COOLDOWN =
  "    directory: '/crates/launcher'\n    schedule:\n      interval: 'weekly'\n    cooldown:\n      default-days: 7\n";
const ACTIONS_COOLDOWN =
  "  - package-ecosystem: 'github-actions'\n    directory: '/'\n    schedule:\n      interval: 'weekly'\n    cooldown:\n      default-days: 7\n";
const HELD =
  "      # Held on purpose: its next major is a different library.\n      - dependency-name: 'held-crate'\n";

const dependabotCases: readonly Case[] = [
  {
    name: 'no rust-toolchain entry',
    files: edit(DEPENDABOT, TOOLCHAIN_ENTRY, ''),
    expect: [POLICY],
  },
  {
    name: 'a three-day cooldown',
    files: edit(
      DEPENDABOT,
      NPM_COOLDOWN,
      NPM_COOLDOWN.replace('default-days: 7', 'default-days: 3'),
    ),
    expect: [POLICY],
  },
  {
    name: 'an ecosystem without a cooldown',
    files: edit(
      DEPENDABOT,
      ACTIONS_COOLDOWN,
      ACTIONS_COOLDOWN.replace('    cooldown:\n      default-days: 7\n', ''),
    ),
    expect: [POLICY],
  },
  {
    name: 'an ignore with no comment above it',
    files: edit(DEPENDABOT, HELD, "      - dependency-name: 'held-crate'\n"),
    expect: [POLICY],
  },
  {
    name: 'an ignore for windows',
    files: edit(
      DEPENDABOT,
      HELD,
      `${HELD}        update-types: ['version-update:semver-major']\n      # An old reason.\n      - dependency-name: 'windows'\n`,
    ),
    expect: [POLICY],
  },
  {
    name: 'a group that also takes majors',
    files: edit(
      DEPENDABOT,
      "      cargo-minor-patch:\n        patterns: ['*']\n        update-types: ['minor', 'patch']\n",
      "      cargo-minor-patch:\n        patterns: ['*']\n        update-types: ['minor', 'patch', 'major']\n",
    ),
    expect: [POLICY],
  },
  {
    name: 'a setting the policy does not know',
    files: edit(DEPENDABOT, NPM_COOLDOWN, `${NPM_COOLDOWN}    open-pull-requests-limit: 5\n`),
    expect: [POLICY],
  },
];

const cases = [...toolchainCases, ...workflowCases, ...pinFileCases, ...dependabotCases];
const failures = cases.map(runCase).filter((line) => line !== null);
for (const failure of failures)
  process.stdout.write(`pin guard self-test: not as expected: ${failure}\n`);
process.stdout.write(
  `pin guard self-test: ${String(cases.length)} cases, ${String(failures.length)} mismatches\n`,
);
process.exitCode = cases.length > 0 && failures.length === 0 ? 0 : 1;
