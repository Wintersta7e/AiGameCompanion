// Self-test of the wiring self-check: a literal clean workflow, step table,
// build script and tracked files, then one literal change per condition, each
// of which must be reported under that condition and no other. Banned words
// are written as parts joined at run time, so this file does not hold them.
//
//   node crates/launcher/scripts/wiring-check.selftest.ts
import {
  aggregateBlock,
  aggregateStatuses,
  checkWiring,
  NON_BLOCKING_JOBS,
  parseWorkflow,
  REQUIRED_JOB_NAMES,
  UnrecognisedShape,
} from './wiring-check.ts';
import type { Workflow } from './wiring-check.ts';

const CHECKOUT = [
  '      - name: Check out the repository',
  '        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1',
  '        with:',
  '          persist-credentials: false',
].join('\n');

const job = (id: string, name: string, runsOn: string, tag: string): string =>
  [
    `  ${id}:`,
    `    name: ${name}`,
    `    runs-on: ${runsOn}`,
    '    timeout-minutes: 10',
    '    permissions:',
    '      contents: read',
    '    defaults:',
    '      run:',
    '        shell: bash',
    '    steps:',
    CHECKOUT,
    '      - name: Register problem matchers',
    '        run: echo "::add-matcher::.github/problem-matchers.json"',
    `      - name: Run the ${tag} rows`,
    `        run: ./scripts/ci-check.sh --job ${tag}`,
    '',
  ].join('\n');

const CLEAN_CI = [
  'name: CI',
  '',
  'on:',
  '  push:',
  '    branches: [main]',
  '  pull_request:',
  '  workflow_dispatch:',
  '  schedule:',
  "    - cron: '23 3 * * 1-6'",
  "    - cron: '23 3 * * 0'",
  '',
  'permissions: {}',
  '',
  'env:',
  '  CARGO_TERM_COLOR: never',
  '  NO_COLOR: "1"',
  '',
  'concurrency:',
  `  group: \${{ github.event_name == 'pull_request' && format('{0}-pr-{1}', github.workflow, github.ref) || format('{0}-run-{1}', github.workflow, github.run_id) }}`,
  `  cancel-in-progress: \${{ github.event_name == 'pull_request' }}`,
  '',
  '# A comment line.',
  'jobs:',
  job('rust', 'Rust (fmt, clippy, test)', 'windows-2025-vs2026', 'rust'),
  job('rust-host', 'Rust (non-Windows build)', 'ubuntu-24.04', 'rust-host'),
  job('frontend', 'Frontend (lint, types, build)', 'ubuntu-24.04', 'frontend'),
  job('release-audit', 'Release build + binary audit', 'ubuntu-24.04', 'release-audit'),
  job('supply-chain', 'Supply chain (cargo-deny)', 'ubuntu-24.04', 'supply-chain'),
  job('workflows', 'Workflows (actionlint)', 'ubuntu-24.04', 'workflows'),
  job('secrets', 'Secrets (gitleaks)', 'ubuntu-24.04', 'secrets'),
  '  aggregate:',
  '    name: Result of the needed CI jobs',
  '    if: always()',
  '    needs: [rust, rust-host, frontend, release-audit, supply-chain, workflows, secrets]',
  '    runs-on: ubuntu-24.04',
  '    timeout-minutes: 5',
  '    permissions: {}',
  '    defaults:',
  '      run:',
  '        shell: bash',
  '    steps:',
  "      - name: Check the needed jobs' results",
  '        env:',
  `          NEEDS_JSON: \${{ toJSON(needs) }}`,
  '        run: |',
  `          count=$(jq 'length' <<<"\${NEEDS_JSON}")`,
  `          failed=$(jq -r '[to_entries[] | select(.value.result != "success") | .key] | join(", ")' <<<"\${NEEDS_JSON}")`,
  `          if [ "\${count}" -lt 1 ]; then echo "no needed CI job reported a result"; exit 1; fi`,
  `          if [ -n "\${failed}" ]; then echo "needed CI jobs that did not succeed: \${failed}"; exit 1; fi`,
  '',
].join('\n');

const row = (name: string, tags: string, category: string, cmd: string): string =>
  ['row', name, tags, category, 'any', '-', '.', '-', '-', '-', 'none', '-', cmd].join('\t');

const CLEAN_LIST = [
  '#kind\tname\ttags\tcategory\ttarget\ttools\tdir\trequires\tevents\tpre\tcount\tzero\tcmd',
  row('fmt', 'rust', 'both', 'cargo fmt --all --check'),
  row('host clippy', 'rust-host', 'both', 'cargo clippy --locked -- -D warnings'),
  row('lint', 'frontend', 'both', 'node node_modules/eslint/bin/eslint.js .'),
  row('audit', 'release-audit', 'both', 'scripts/binary-audit.sh self-test'),
  row('deny', 'supply-chain', 'both', 'cargo deny --locked check'),
  row('actionlint', 'workflows', 'both', 'actionlint'),
  row('scan', 'secrets', 'both', 'gitleaks version'),
  'rows\t7',
  '',
].join('\n');

const PR_TEXT = [
  'name: PR text',
  'on:',
  '  pull_request:',
  '    types: [opened, edited, synchronize, reopened]',
  'permissions: {}',
  'env:',
  '  CARGO_TERM_COLOR: never',
  "  NO_COLOR: '1'",
  'concurrency:',
  `  group: pr-text-\${{ github.event.pull_request.number }}`,
  '  cancel-in-progress: true',
  'jobs:',
  job('pr-text', 'PR title, body and branch name', 'ubuntu-24.04', 'pr-text'),
].join('\n');

const PR_TEXT_LIST = CLEAN_LIST.replace(
  'rows\t7',
  `${row('pr text', 'pr-text', 'ci-only', 'node scripts/pr.ts')}\nrows\t8`,
);

const EIGHT_NAMES = [
  'Rust (fmt, clippy, test)',
  'Rust (non-Windows build)',
  'Frontend (lint, types, build)',
  'Release build + binary audit',
  'Supply chain (cargo-deny)',
  'Workflows (actionlint)',
  'Secrets (gitleaks)',
  'Result of the needed CI jobs',
];

// Replaces `from` (which must occur exactly `times` times) with `to`.
const swap = (text: string, from: string, to: string, times = 1): string => {
  const found = text.split(from).length - 1;
  if (found !== times) throw new Error(`the literal "${from}" occurs ${found} times, not ${times}`);
  return text.replaceAll(from, to);
};

interface Case {
  readonly name: string;
  readonly files: Readonly<Record<string, string>>;
  readonly list?: string;
  readonly build?: string;
  // Tracked files besides the workflow files; replaces CLEAN_TRACKED's entry of the same path.
  readonly tracked?: Readonly<Record<string, string>>;
  readonly expect: readonly string[];
  // A part the counts line must hold.
  readonly counts?: string;
}

const j = (...parts: readonly string[]): string => parts.join('');

const CLEAN_BUILD = [
  '#!/usr/bin/env bash',
  '# Builds the release; cargo xwin reads the lock.',
  'cargo xwin build -p launcher --release \\',
  '  --target x86_64-pc-windows-msvc --locked',
  'TARGET_DIR=$(cargo metadata --no-deps --format-version 1 --locked | jq -r .target_directory)',
  '',
].join('\n');

const CLEAN_TRACKED: Readonly<Record<string, string>> = {
  'README.md': 'Lint and test gates: ./scripts/ci-check.sh (every check this machine can run).\n',
  'scripts/ci-check.sh': '#!/usr/bin/env bash\nnode node_modules/eslint/bin/eslint.js .\n',
  'CHANGELOG.md': j('- Replaced cargo-mach', 'ete with a rustc lint.\n'),
  'scripts/test-baselines/linux.list': j('overlay::tests::type_', 'mirrors_rust run\n'),
};

const ci = (text: string): Readonly<Record<string, string>> => ({
  '.github/workflows/ci.yml': text,
});

const conditionCases: readonly Case[] = [
  { name: 'the clean workflow passes', files: ci(CLEAN_CI), expect: [] },
  { name: 'no workflow file', files: {}, expect: ['empty'] },
  {
    name: 'no checkout',
    files: ci(
      swap(
        swap(
          CLEAN_CI,
          CHECKOUT,
          '      - name: Check out the repository\n        run: git --version',
          7,
        ),
        '    permissions:\n      contents: read\n',
        '    permissions: {}\n',
        7,
      ),
    ),
    expect: ['empty'],
  },
  {
    name: 'a runner call with an if',
    files: ci(
      swap(
        CLEAN_CI,
        '        run: ./scripts/ci-check.sh --job rust\n',
        '        if: always()\n        run: ./scripts/ci-check.sh --job rust\n',
      ),
    ),
    expect: ['runner call'],
  },
  {
    name: 'a call to a tag the table lacks',
    files: ci(swap(CLEAN_CI, '--job secrets', '--job secret')),
    expect: ['tag callers'],
  },
  {
    name: 'an aggregate without if: always()',
    files: ci(swap(CLEAN_CI, '    if: always()\n', '')),
    expect: ['aggregate'],
  },
  {
    name: 'a job-level if on a needed job',
    files: ci(
      swap(
        CLEAN_CI,
        '    name: Rust (fmt, clippy, test)\n',
        "    name: Rust (fmt, clippy, test)\n    if: github.event_name == 'push'\n",
      ),
    ),
    expect: ['job if'],
  },
  {
    name: 'a job without a timeout',
    files: ci(
      swap(
        CLEAN_CI,
        '    name: Secrets (gitleaks)\n    runs-on: ubuntu-24.04\n    timeout-minutes: 10\n',
        '    name: Secrets (gitleaks)\n    runs-on: ubuntu-24.04\n',
      ),
    ),
    expect: ['job shape'],
  },
  {
    name: 'continue-on-error on a step',
    files: ci(
      swap(
        CLEAN_CI,
        '      - name: Run the frontend rows\n',
        '      - name: Say hello\n        run: echo hello\n        continue-on-error: true\n      - name: Run the frontend rows\n',
      ),
    ),
    expect: ['escape'],
  },
  {
    name: 'no workflow_dispatch trigger',
    files: ci(swap(CLEAN_CI, '  workflow_dispatch:\n', '')),
    expect: ['triggers'],
  },
  {
    name: 'colour left on',
    files: ci(swap(CLEAN_CI, '  NO_COLOR: "1"\n', '')),
    expect: ['colour'],
  },
  {
    name: 'a non-blocking row in a needed job',
    files: ci(CLEAN_CI),
    list: swap(
      CLEAN_LIST,
      row('scan', 'secrets', 'both', 'gitleaks version'),
      row('scan', 'secrets', 'non-blocking', 'gitleaks version'),
    ),
    expect: ['non-blocking'],
  },
  {
    name: 'an aggregate block that always passes',
    files: ci(
      swap(
        CLEAN_CI,
        `          if [ -n "\${failed}" ]; then echo "needed CI jobs that did not succeed: \${failed}"; exit 1; fi\n`,
        '          exit 0\n',
      ),
    ),
    expect: ['payloads'],
  },
  {
    name: 'an unrecognised key in another workflow',
    files: {
      ...ci(CLEAN_CI),
      '.github/workflows/other.yml':
        'name: Other\njobs:\n  one:\n    strategy:\n      fail-fast: false\n',
    },
    expect: ['shape'],
  },
  {
    name: 'an anchor in another workflow',
    files: { ...ci(CLEAN_CI), '.github/workflows/other.yml': 'name: &n Other\n' },
    expect: ['shape'],
  },
  {
    name: 'the PR text workflow passes',
    files: { ...ci(CLEAN_CI), '.github/workflows/pr-text.yml': PR_TEXT },
    list: PR_TEXT_LIST,
    expect: [],
  },
  {
    name: 'a PR text workflow that an edited title does not re-run',
    files: {
      ...ci(CLEAN_CI),
      '.github/workflows/pr-text.yml': swap(
        PR_TEXT,
        'types: [opened, edited, synchronize, reopened]',
        'types: [opened, synchronize, reopened]',
      ),
    },
    list: PR_TEXT_LIST,
    expect: ['triggers'],
  },
];

// The four jobs that never gate a merge: two beta canaries and the freshness
// report run weekly or by hand; the coverage job runs on every event.
const WEEKLY = "'23 3 * * 0'";
const DAILY = "'23 3 * * 1-6'";
const ifLine = (cron: string): string =>
  `    if: github.event_name == 'workflow_dispatch' || github.event.schedule == ${cron}\n`;
const BETA_ENV = '    env:\n      RUSTUP_TOOLCHAIN: beta\n';
const TOKEN_ENV = `        env:\n          GH_TOKEN: \${{ github.token }}\n`;
const extraJob = (id: string, tag: string, head: string): string =>
  swap(
    job(id, `The ${id} job`, 'ubuntu-24.04', tag),
    `    name: The ${id} job\n`,
    `    name: The ${id} job\n${head}`,
  );
const CANARY_LINUX = extraJob('canary-linux', 'rust-host', `${ifLine(WEEKLY)}${BETA_ENV}`);
const CANARY_WINDOWS = extraJob('canary-windows', 'rust', `${ifLine(WEEKLY)}${BETA_ENV}`);
const FRESHNESS_CALL = '        run: ./scripts/ci-check.sh --job freshness\n';
const freshnessJob = (head: string): string =>
  swap(extraJob('freshness', 'freshness', head), FRESHNESS_CALL, `${TOKEN_ENV}${FRESHNESS_CALL}`);
const FRESHNESS = freshnessJob(ifLine(WEEKLY));
const COVERAGE = extraJob('coverage', 'coverage', '');

const fullCi = (
  linux = CANARY_LINUX,
  windows = CANARY_WINDOWS,
  freshness = FRESHNESS,
  coverage = COVERAGE,
): string =>
  swap(CLEAN_CI, '  aggregate:\n', `${linux}${windows}${freshness}${coverage}  aggregate:\n`);

const FULL_LIST = swap(
  CLEAN_LIST,
  'rows\t7\n',
  [
    row('vite build', 'rust,coverage', 'both', 'node node_modules/vite/bin/vite.js build'),
    row('coverage report', 'coverage', 'non-blocking', 'cargo llvm-cov --workspace --locked'),
    row('freshness report', 'freshness', 'non-blocking', 'node scripts/freshness.ts'),
    'rows\t10\n',
  ].join('\n'),
);
const NEEDS =
  '    needs: [rust, rust-host, frontend, release-audit, supply-chain, workflows, secrets]\n';
const full = (
  name: string,
  text: string,
  expect: readonly string[],
  extra: Partial<Case> = {},
): Case => ({ name, files: ci(text), list: FULL_LIST, expect, ...extra });

const OTHER_WORKFLOW = [
  'name: Other',
  '',
  'on:',
  '  workflow_dispatch:',
  '',
  'permissions: {}',
  '',
  'env:',
  '  CARGO_TERM_COLOR: never',
  '  NO_COLOR: "1"',
  '',
  'jobs:',
  job('extra', 'Extra', 'ubuntu-24.04', 'extra'),
].join('\n');

const nonBlockingCases: readonly Case[] = [
  full('the four non-blocking jobs pass', fullCi(), [], { counts: 'non-blocking jobs 4' }),
  full(
    'the aggregate needing canary-linux',
    swap(fullCi(), NEEDS, swap(NEEDS, 'secrets]', 'secrets, canary-linux]')),
    ['aggregate'],
  ),
  full(
    'the aggregate needing coverage',
    swap(fullCi(), NEEDS, swap(NEEDS, 'secrets]', 'secrets, coverage]')),
    ['aggregate'],
  ),
  full('freshness without an if', fullCi(undefined, undefined, freshnessJob('')), ['job if']),
  full(
    'a canary whose if names the daily cron line',
    fullCi(undefined, extraJob('canary-windows', 'rust', `${ifLine(DAILY)}${BETA_ENV}`)),
    ['job if'],
  ),
  full(
    'a canary whose if names a cron absent from on.schedule',
    fullCi(extraJob('canary-linux', 'rust-host', `${ifLine("'23 4 * * 0'")}${BETA_ENV}`)),
    ['job if'],
  ),
  full(
    'the two canaries naming different weekly crons',
    swap(
      fullCi(undefined, extraJob('canary-windows', 'rust', `${ifLine("'23 3 * * 6'")}${BETA_ENV}`)),
      `    - cron: ${DAILY}\n`,
      "    - cron: '23 3 * * 6'\n",
    ),
    ['job if'],
  ),
  full(
    'a canary whose if is another expression',
    fullCi(
      extraJob('canary-linux', 'rust-host', `    if: github.event_name == 'schedule'\n${BETA_ENV}`),
    ),
    ['job if'],
  ),
  full(
    'coverage with an if',
    fullCi(undefined, undefined, undefined, extraJob('coverage', 'coverage', ifLine(WEEKLY))),
    ['job if'],
  ),
  full(
    'the toolchain override in the host Rust job',
    swap(
      fullCi(),
      '    name: Rust (non-Windows build)\n',
      `    name: Rust (non-Windows build)\n${BETA_ENV}`,
    ),
    ['escape'],
  ),
  full('the toolchain override in a job of another workflow', fullCi(), ['escape'], {
    files: {
      ...ci(fullCi()),
      '.github/workflows/other.yml': swap(
        OTHER_WORKFLOW,
        '    name: Extra\n',
        `    name: Extra\n${BETA_ENV}`,
      ),
    },
    list: swap(FULL_LIST, 'rows\t10\n', `${row('extra', 'extra', 'both', 'true')}\nrows\t11\n`),
  }),
  full(
    'the toolchain override for every job of ci.yml',
    swap(fullCi(), '  NO_COLOR: "1"\n', '  NO_COLOR: "1"\n  RUSTUP_TOOLCHAIN: beta\n'),
    ['escape'],
  ),
  // The freshness row joins the rust tag: rust gains a non-blocking row, so it
  // may have only one caller, and that row is now called by a needed job.
  full(
    'a freshness row that also carries the rust tag',
    fullCi(),
    ['tag callers', 'non-blocking'],
    {
      list: swap(
        FULL_LIST,
        row('freshness report', 'freshness', 'non-blocking', 'node scripts/freshness.ts'),
        row('freshness report', 'freshness,rust', 'non-blocking', 'node scripts/freshness.ts'),
      ),
    },
  ),
  // The Windows Rust job's one call moves to coverage: coverage has two callers,
  // one of them needed, and rust keeps only its canary.
  full(
    'the coverage tag also called by the Windows Rust job',
    swap(
      fullCi(),
      job('rust', 'Rust (fmt, clippy, test)', 'windows-2025-vs2026', 'rust'),
      job('rust', 'Rust (fmt, clippy, test)', 'windows-2025-vs2026', 'coverage'),
    ),
    ['tag callers', 'non-blocking'],
  ),
  full(
    'the coverage tag in the table with no caller',
    fullCi(undefined, undefined, undefined, ''),
    ['tag callers'],
  ),
  full(
    'the job token on the freshness checkout',
    fullCi(undefined, undefined, swap(FRESHNESS, `${CHECKOUT}\n`, `${CHECKOUT}\n${TOKEN_ENV}`)),
    ['permissions'],
  ),
  full(
    'the job token on the coverage runner call',
    fullCi(
      undefined,
      undefined,
      undefined,
      swap(
        COVERAGE,
        '        run: ./scripts/ci-check.sh --job coverage\n',
        `${TOKEN_ENV}        run: ./scripts/ci-check.sh --job coverage\n`,
      ),
    ),
    ['permissions'],
  ),
  full(
    'a freshness runner call without the job token',
    fullCi(undefined, undefined, extraJob('freshness', 'freshness', ifLine(WEEKLY))),
    ['permissions'],
  ),
];

const nameCases: readonly Case[] = EIGHT_NAMES.map((name) => ({
  name: `the required name "${name}" renamed`,
  files: ci(swap(CLEAN_CI, `    name: ${name}\n`, `    name: ${name} renamed\n`)),
  expect: ['names'],
}));

const statusCache = new Map<string, Promise<number[] | null>>();

const statusesFor = (workflows: readonly Workflow[]): Promise<number[] | null> => {
  const block = aggregateBlock(workflows);
  const key = block ?? '';
  const cached = statusCache.get(key) ?? aggregateStatuses(block);
  statusCache.set(key, cached);
  return cached;
};

const runCase = async (c: Case): Promise<string | null> => {
  const workflows: Workflow[] = [];
  const unrecognised: string[] = [];
  for (const [file, text] of Object.entries(c.files)) {
    try {
      workflows.push(parseWorkflow(text, file));
    } catch (error: unknown) {
      if (!(error instanceof UnrecognisedShape)) throw error;
      unrecognised.push(error.message);
    }
  }
  const tracked = { ...CLEAN_TRACKED, ...c.files, ...(c.tracked ?? {}) };
  const result = checkWiring({
    workflows,
    unrecognised,
    listText: c.list ?? CLEAN_LIST,
    aggregateStatuses: await statusesFor(workflows),
    requiredNames: EIGHT_NAMES,
    buildScript: c.build ?? CLEAN_BUILD,
    trackedFiles: Object.entries(tracked).map(([file, text]) => ({ path: file, text })),
  });
  const got = [...new Set(result.failures.map((failure) => failure.condition))].sort();
  const want = [...c.expect].sort();
  const countsOk = c.counts === undefined || result.counts.includes(c.counts);
  if (got.join('|') === want.join('|') && countsOk) return null;
  const detail = result.failures.map((failure) => failure.message).join('; ');
  return `${c.name}: reported [${got.join(', ')}], expected [${want.join(', ')}] (${detail}; ${result.counts})`;
};

const constantCases = (): (string | null)[] => [
  REQUIRED_JOB_NAMES.join('|') === EIGHT_NAMES.join('|')
    ? null
    : 'the required job names are not the eight contexts',
  NON_BLOCKING_JOBS.join('|') ===
  ['canary-linux', 'canary-windows', 'freshness', 'coverage'].join('|')
    ? null
    : 'the non-blocking job list is not the four scheduled or measure-only jobs',
];

const SECRETS_JOB = job('secrets', 'Secrets (gitleaks)', 'ubuntu-24.04', 'secrets');
const WORKFLOWS_JOB = job('workflows', 'Workflows (actionlint)', 'ubuntu-24.04', 'workflows');
const secretsJob = (from: string, to: string): Readonly<Record<string, string>> =>
  ci(swap(CLEAN_CI, SECRETS_JOB, swap(SECRETS_JOB, from, to)));
const PERMS = '    permissions:\n      contents: read\n';
const MATCHER = '      - name: Register problem matchers\n';
const CALL_SECRETS = '        run: ./scripts/ci-check.sh --job secrets\n';

const lockedCases: readonly Case[] = [
  {
    name: 'the clean build script and rows are counted',
    files: ci(CLEAN_CI),
    expect: [],
    counts: 'cargo invocations 4',
  },
  {
    name: 'a row without --locked',
    files: ci(CLEAN_CI),
    list: swap(
      CLEAN_LIST,
      'cargo clippy --locked -- -D warnings',
      'cargo clippy --workspace -- -D warnings',
    ),
    expect: ['locked'],
  },
  {
    name: 'a build.sh line without --locked',
    files: ci(CLEAN_CI),
    build: 'cargo xwin build --release\n',
    expect: ['locked'],
  },
  {
    name: 'no cargo call anywhere',
    files: ci(CLEAN_CI),
    list: swap(
      swap(CLEAN_LIST, 'cargo clippy --locked -- -D warnings', 'true'),
      'cargo deny --locked check',
      'true',
    ),
    build: '#!/usr/bin/env bash\necho nothing to build\n',
    expect: ['locked'],
  },
];

const permissionCases: readonly Case[] = [
  {
    name: 'top-level contents: read',
    files: ci(swap(CLEAN_CI, 'permissions: {}\n\nenv:', 'permissions:\n  contents: read\n\nenv:')),
    expect: ['permissions'],
  },
  {
    name: 'a job with no permissions block',
    files: secretsJob(PERMS, ''),
    expect: ['permissions'],
  },
  {
    name: 'contents: write',
    files: secretsJob(PERMS, '    permissions:\n      contents: write\n'),
    expect: ['permissions'],
  },
  {
    name: 'permissions: write-all',
    files: secretsJob(PERMS, '    permissions: write-all\n'),
    expect: ['permissions'],
  },
  {
    name: 'permissions: read-all',
    files: secretsJob(PERMS, '    permissions: read-all\n'),
    expect: ['permissions'],
  },
  {
    name: 'an extra scope',
    files: secretsJob(PERMS, '    permissions:\n      contents: read\n      pull-requests: read\n'),
    expect: ['permissions'],
  },
  {
    name: 'contents: read without a checkout',
    files: secretsJob(
      CHECKOUT,
      '      - name: Check out the repository\n        run: git --version',
    ),
    expect: ['permissions'],
  },
  {
    name: 'an aggregate job with a scope',
    files: ci(
      swap(
        CLEAN_CI,
        '    timeout-minutes: 5\n    permissions: {}\n',
        `    timeout-minutes: 5\n${PERMS}`,
      ),
    ),
    expect: ['aggregate', 'permissions'],
  },
  {
    name: 'a checkout that keeps the credentials',
    files: secretsJob('        with:\n          persist-credentials: false\n', ''),
    expect: ['permissions'],
  },
  {
    name: 'the job token on a setup step',
    files: secretsJob(
      MATCHER,
      `${MATCHER}        env:\n          GH_TOKEN: \${{ github.token }}\n`,
    ),
    expect: ['permissions'],
  },
  {
    name: 'a secrets. expression',
    files: secretsJob(
      CALL_SECRETS,
      `        env:\n          KEY: \${{ ${j('sec', 'rets')}.KEY }}\n${CALL_SECRETS}`,
    ),
    expect: ['permissions'],
  },
  {
    name: 'the job token on the workflow-lint call',
    files: ci(
      swap(
        CLEAN_CI,
        WORKFLOWS_JOB,
        swap(
          WORKFLOWS_JOB,
          '        run: ./scripts/ci-check.sh --job workflows\n',
          `        env:\n          GH_TOKEN: \${{ github.token }}\n        run: ./scripts/ci-check.sh --job workflows\n`,
        ),
      ),
    ),
    expect: [],
    counts: 'token steps 1',
  },
];

const banned = (
  name: string,
  tracked: Readonly<Record<string, string>>,
  expect: readonly string[],
): Case => ({
  name,
  files: ci(CLEAN_CI),
  tracked,
  expect,
});
const hit = ['banned text'];

const bannedCases: readonly Case[] = [
  banned(
    'a fetching front door in the runner',
    { 'scripts/ci-check.sh': j('np', 'x eslint .\n') },
    hit,
  ),
  banned(
    'a bin link in the runner',
    { 'scripts/ci-check.sh': j('node_modules/', '.bin/eslint .\n') },
    hit,
  ),
  banned(
    'a heap flag in a workflow',
    { '.github/workflows/extra.yml': j('env:\n  NODE_', 'OPTIONS: x\n') },
    hit,
  ),
  banned(
    'an artifact upload in a workflow',
    { '.github/workflows/extra.yml': j('uses: actions/upload-', 'artifact@v7\n') },
    hit,
  ),
  ...[
    j('mir', 'rors CI'),
    j('the same ', 'checks'),
    j('same as ', 'CI'),
    j('pari', 'ty'),
    j('safe to ', 'push'),
    j('ALL ', 'GREEN'),
    j('the shipped ', 'artifact'),
    j('users actually ', 'download'),
  ].map((claim) => banned(`the claim "${claim}"`, { 'README.md': `The gate ${claim}.\n` }, hit)),
  banned(
    'the retired tool in a source file',
    { 'crates/launcher/src/a.rs': j('// cargo-mach', 'ete\n') },
    hit,
  ),
  banned(
    'a review action in a workflow',
    { '.github/workflows/extra.yml': j('uses: actions/dependency-review-', 'action@v4\n') },
    hit,
  ),
  banned('an advisory tool in a script', { 'scripts/extra.sh': j('cargo ', 'audit\n') }, hit),
  banned('a scanner in a script', { 'scripts/extra.sh': j('osv-', 'scanner scan\n') }, hit),
  banned(
    'a lockfile linter in a workflow',
    { '.github/workflows/extra.yml': j('run: lockfile-', 'lint\n') },
    hit,
  ),
  banned(
    'signature checks in the runner',
    { 'scripts/ci-check.sh': j('npm audit ', 'signatures\n') },
    hit,
  ),
  {
    name: 'an advisory tool in a row',
    files: ci(CLEAN_CI),
    list: swap(CLEAN_LIST, 'gitleaks version', j('cargo ', 'audit')),
    expect: hit,
  },
  banned(
    'a fetching front door outside its scope',
    { 'crates/launcher/src/a.ts': j('// np', 'x\n') },
    [],
  ),
  banned('a claim outside its scope', { 'crates/launcher/src/a.ts': j('// all ', 'green\n') }, []),
  banned(
    'an artifact upload outside a workflow',
    { 'scripts/notes.txt': j('upload-', 'artifact\n') },
    [],
  ),
  banned(
    'an advisory tool outside its scope',
    { 'crates/launcher/notes.md': j('cargo ', 'audit\n') },
    [],
  ),
];

const cases = [
  ...conditionCases,
  ...nonBlockingCases,
  ...nameCases,
  ...lockedCases,
  ...permissionCases,
  ...bannedCases,
];
const results = [...(await Promise.all(cases.map(runCase))), ...constantCases()];
const failures = results.filter((line) => line !== null);
for (const failure of failures)
  process.stdout.write(`wiring self-check self-test: not as expected: ${failure}\n`);
const total = results.length;
process.stdout.write(
  `wiring self-check self-test: cases ${total}, as expected ${total - failures.length}\n`,
);
process.exitCode = total > 0 && failures.length === 0 ? 0 : 1;
