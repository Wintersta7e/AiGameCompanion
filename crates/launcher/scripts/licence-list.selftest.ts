// Self-test of the licence list check: literal deny.toml, review config and
// npm lockfile texts, each with the verdict it must get.
//
//   node crates/launcher/scripts/licence-list.selftest.ts
import { check } from './licence-list.ts';

interface Case {
  readonly name: string;
  readonly deny?: string;
  readonly config?: string;
  readonly lock?: string;
  readonly expect: number;
  // A line the checker must print.
  readonly says: string;
}

const DENY = [
  '[advisories]',
  'ignore = []',
  '',
  '[licenses]',
  'version = 2',
  'allow = [',
  '    "MIT",',
  '    "Apache-2.0 WITH LLVM-exception",',
  ']',
  'exceptions = []',
  '',
  '[bans]',
  'allow = ["not-a-licence"]',
  '',
].join('\n');

const config = (licences: readonly string[]): string =>
  [
    'fail-on-severity: low',
    'license-check: true',
    'allow-licenses:',
    ...licences.map((licence) => `  - ${licence}`),
    'comment-summary-in-pr: never',
    '',
  ].join('\n');

const CONFIG = config(['MIT', 'Apache-2.0 WITH LLVM-exception', 'BSD-2-Clause']);

const lock = (licences: readonly string[]): string =>
  JSON.stringify({
    name: 'planted',
    lockfileVersion: 3,
    packages: {
      '': { name: 'planted', license: 'MIT' },
      ...Object.fromEntries(
        licences.map((licence, index) => [`node_modules/p${String(index)}`, { license: licence }]),
      ),
    },
  });

const LOCK = lock(['MIT', 'BSD-2-Clause']);

const cases: readonly Case[] = [
  {
    name: 'the config is deny.toml plus the npm-only entries',
    expect: 0,
    says: 'licence list: deny 2 config 3 npm-only 1 npm packages 2',
  },
  {
    name: 'a deny.toml entry missing from the config',
    config: config(['MIT', 'BSD-2-Clause']),
    expect: 1,
    says: 'FAIL the deny.toml licence Apache-2.0 WITH LLVM-exception is not in the review config',
  },
  {
    name: 'an npm-only entry no package uses',
    config: config(['MIT', 'Apache-2.0 WITH LLVM-exception', 'BSD-2-Clause', '0BSD']),
    expect: 1,
    says: 'FAIL the review config allows 0BSD, which is not in deny.toml and no npm package uses',
  },
  {
    name: 'OR, AND and parenthesised expressions are split',
    config: config(['MIT', 'Apache-2.0 WITH LLVM-exception', 'BSD-2-Clause', '0BSD']),
    lock: lock(['(MIT OR BSD-2-Clause)', 'ISC AND (0BSD OR MIT)']),
    expect: 0,
    says: 'licence list: deny 2 config 4 npm-only 2 npm packages 2',
  },
  {
    name: 'zero npm packages',
    lock: lock([]),
    expect: 1,
    says: 'FAIL the npm lockfile lists no package',
  },
  {
    name: 'a config without allow-licenses',
    config: 'fail-on-severity: low\n',
    expect: 1,
    says: 'FAIL the review config has no allow-licenses list',
  },
  {
    name: 'a deny.toml without a licence allow list',
    deny: '[advisories]\nignore = []\n',
    expect: 1,
    says: 'FAIL deny.toml has no [licenses] allow array',
  },
  { name: 'a malformed lockfile', lock: '{"packages": ', expect: 1, says: 'is not JSON' },
];

const failures: string[] = [];
for (const c of cases) {
  const outcome = check(c.deny ?? DENY, c.config ?? CONFIG, c.lock ?? LOCK);
  const problems: string[] = [];
  if (outcome.code !== c.expect)
    problems.push(`exit ${String(outcome.code)}, expected ${String(c.expect)}`);
  if (!outcome.out.includes(c.says)) problems.push(`no output holding "${c.says}"`);
  if (problems.length > 0) failures.push(`${c.name}: ${problems.join('; ')}`);
}
for (const failure of failures)
  process.stdout.write(`licence list self-test: not as expected: ${failure}\n`);
const expected = cases.length - failures.length;
process.stdout.write(
  `licence list self-test: cases ${String(cases.length)}, as expected ${String(expected)}\n`,
);
process.exitCode = cases.length > 0 && expected === cases.length ? 0 : 1;
