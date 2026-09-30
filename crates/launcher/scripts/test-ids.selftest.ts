// Self-test of the test identity checker: literal runs and baselines, each
// with the verdict it must get. PR cases build throwaway git repositories.
//
//   node crates/launcher/scripts/test-ids.selftest.ts
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { run } from './test-ids.ts';
import type { Mode } from './test-ids.ts';

interface Case {
  readonly name: string;
  readonly mode: Mode;
  // The run's output, as the test runner printed it.
  readonly input: string;
  // The baseline file's content; null leaves the file absent.
  readonly baseline: string | null;
  readonly env?: Readonly<Record<string, string>>;
  // Commits made in a throwaway repository before the run: 0 = no repository.
  readonly commits?: number;
  readonly expect: number;
  // A line the checker must print.
  readonly says?: string;
  // The baseline must be byte-identical after the run.
  readonly unchanged?: boolean;
}

const libtest = (lines: readonly string[], summary: string): string =>
  ['running 3 tests', ...lines, '', `test result: ${summary}`, ''].join('\n');

const ok3 = libtest(
  ['test a::one ... ok', 'test a::two ... ok', 'test b::three ... ignored'],
  'ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s',
);
const base3 = 'a::one run\na::two run\nb::three ignored\n';

const tapTest = (indent: string, n: number, name: string, type: string): string[] => [
  `${indent}# Subtest: ${name.replace(/ # (SKIP|TODO)$/u, '')}`,
  `${indent}ok ${n} - ${name}`,
  `${indent}  ---`,
  `${indent}  duration_ms: 0.1`,
  `${indent}  type: '${type}'`,
  `${indent}  ...`,
];

const tapRun = (body: readonly string[], tests: number): string =>
  [
    'TAP version 13',
    ...body,
    `# tests ${tests}`,
    '# suites 0',
    `# pass ${tests}`,
    '# fail 0',
    '# cancelled 0',
    '',
  ].join('\n');

const planted: readonly Case[] = [
  { name: 'empty input', mode: 'libtest', input: '', baseline: '', expect: 1, says: 'no test id' },
  {
    name: 'a duplicated libtest id',
    mode: 'libtest',
    input: libtest(
      ['test a::one ... ok', 'test a::one ... ok'],
      'ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out',
    ),
    baseline: 'a::one run\n',
    expect: 1,
    says: 'occur twice: a::one',
  },
  {
    name: 'one TAP name in two files',
    mode: 'tap',
    input: tapRun([...tapTest('', 1, 'formats', 'test'), ...tapTest('', 2, 'formats', 'test')], 2),
    baseline: 'formats run\n',
    expect: 1,
    says: 'occur twice: formats',
  },
  {
    name: 'a filtered-out test',
    mode: 'libtest',
    input: libtest(
      ['test a::one ... ok'],
      'ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out',
    ),
    baseline: 'a::one run\n',
    expect: 1,
    says: '1 tests filtered out',
  },
  {
    name: 'a summary above the ids',
    mode: 'libtest',
    input: libtest(
      ['test a::one ... ok'],
      'ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out',
    ),
    baseline: 'a::one run\n',
    expect: 1,
    says: 'the summary counts 2 tests but 1 ids',
  },
  {
    name: 'no summary line',
    mode: 'libtest',
    input: 'test a::one ... ok\n',
    baseline: 'a::one run\n',
    expect: 1,
    says: 'no summary line',
  },
  {
    name: 'a failed test',
    mode: 'libtest',
    input: libtest(
      ['test a::one ... FAILED'],
      'FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out',
    ),
    baseline: 'a::one run\n',
    expect: 1,
    says: '1 tests failed: a::one',
  },
  {
    name: 'a TAP summary above the ids',
    mode: 'tap',
    input: tapRun(tapTest('', 1, 'formats', 'test'), 2),
    baseline: 'formats run\n',
    expect: 1,
    says: 'the summary counts 2 tests but 1 ids',
  },
  {
    name: 'a TAP run of 0 tests',
    mode: 'tap',
    input: tapRun([], 0),
    baseline: '',
    expect: 1,
    says: 'no test id',
  },
  {
    name: 'a missing id and a re-statused id',
    mode: 'libtest',
    input: libtest(
      ['test a::one ... ok', 'test b::three ... ignored', 'test a::two ... ignored'],
      'ok. 1 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out',
    ),
    baseline: 'a::one run\na::two run\nb::three ignored\nc::gone run\n',
    expect: 1,
    says: '-c::gone run',
  },
  {
    name: 'an unsorted baseline',
    mode: 'libtest',
    input: ok3,
    baseline: 'a::two run\na::one run\nb::three ignored\n',
    expect: 1,
    says: 'not in canonical form',
  },
  {
    name: 'an ignored run missing the ignored id',
    mode: 'libtest-ignored',
    input: libtest([], 'ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out'),
    baseline: base3,
    expect: 1,
    says: "ignored run's ids differ",
  },
  {
    name: 'a pull request whose base commit is unreadable',
    mode: 'libtest',
    input: ok3,
    baseline: base3,
    env: { GITHUB_EVENT_NAME: 'pull_request' },
    commits: 1,
    expect: 1,
    says: 'unreadable',
  },
  {
    name: 'an update in CI',
    mode: 'libtest',
    input: ok3,
    baseline: 'a::one run\n',
    env: { CI_CHECK_UPDATE_BASELINES: '1', GITHUB_ACTIONS: 'true' },
    expect: 2,
    unchanged: true,
  },
];

const clean: readonly Case[] = [
  {
    name: 'a CRLF run',
    mode: 'libtest',
    input: ok3.replaceAll('\n', '\r\n'),
    baseline: base3,
    expect: 0,
  },
  {
    name: 'a doc-test id',
    mode: 'libtest',
    input: libtest(
      ['test src/lib.rs - f (line 3) ... ok'],
      'ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out',
    ),
    baseline: 'src/lib.rs - f (line 3) run\n',
    expect: 0,
  },
  {
    name: 'an ignore reason',
    mode: 'libtest',
    input: libtest(
      ['test w::x ... ignored, needs WSL'],
      'ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out',
    ),
    baseline: 'w::x ignored\n',
    expect: 0,
  },
  {
    name: 'a suite holding a test with two subtests',
    mode: 'tap',
    input: [
      'TAP version 13',
      '# Subtest: format',
      '    # Subtest: parent',
      '        # Subtest: child 1',
      '        ok 1 - child 1',
      '          ---',
      '          duration_ms: 0.1',
      "          type: 'test'",
      '          ...',
      '        # Subtest: child 2',
      '        ok 2 - child 2',
      '          ---',
      '          duration_ms: 0.1',
      "          type: 'test'",
      '          ...',
      '        1..2',
      '    ok 1 - parent',
      '      ---',
      '      duration_ms: 0.3',
      "      type: 'test'",
      '      ...',
      '    1..1',
      'ok 1 - format',
      '  ---',
      '  duration_ms: 0.4',
      "  type: 'suite'",
      '  ...',
      '1..1',
      '# tests 3',
      '# suites 1',
      '# pass 3',
      '# fail 0',
      '# cancelled 0',
      '',
    ].join('\n'),
    baseline: 'format > parent > child 1 run\nformat > parent > child 2 run\nformat > parent run\n',
    expect: 0,
  },
  {
    name: 'a skipped TAP test',
    mode: 'tap',
    input: tapRun(tapTest('', 1, 'later # SKIP', 'test'), 1),
    baseline: 'later skip\n',
    expect: 0,
  },
  {
    name: 'a TAP test to do',
    mode: 'tap',
    input: tapRun(tapTest('', 1, 'someday # TODO', 'test'), 1),
    baseline: 'someday todo\n',
    expect: 0,
  },
  {
    name: 'a pull request whose base has no baseline',
    mode: 'libtest',
    input: ok3,
    baseline: base3,
    env: { GITHUB_EVENT_NAME: 'pull_request' },
    commits: 2,
    expect: 0,
    says: 'ids changed since the base commit',
  },
];

const git = (cwd: string, args: readonly string[]): void => {
  execFileSync('git', args, {
    cwd,
    stdio: 'ignore',
    env: {
      ...process.env,
      GIT_CONFIG_GLOBAL: '/dev/null',
      GIT_CONFIG_NOSYSTEM: '1',
      GIT_AUTHOR_NAME: 'Id Test',
      GIT_AUTHOR_EMAIL: 'id-test@example.com',
      GIT_COMMITTER_NAME: 'Id Test',
      GIT_COMMITTER_EMAIL: 'id-test@example.com',
    },
  });
};

// Commits that never touch the baseline file, so the base commit has none.
const makeRepo = (dir: string, commits: number): void => {
  git(dir, ['init', '-q', '-b', 'main']);
  for (let i = 1; i <= commits; i += 1) {
    writeFileSync(path.join(dir, 'notes.txt'), `commit ${i}\n`);
    git(dir, ['add', 'notes.txt']);
    git(dir, ['commit', '-q', '-m', `commit ${i}`]);
  }
};

async function* linesOf(text: string): AsyncGenerator<string> {
  const lines = text.split('\n');
  if (lines.at(-1) === '') lines.pop();
  yield* lines;
  await Promise.resolve();
}

const runCase = async (c: Case): Promise<string | null> => {
  const dir = mkdtempSync(path.join(tmpdir(), 'test-ids-'));
  try {
    if ((c.commits ?? 0) > 0) makeRepo(dir, c.commits ?? 0);
    const file = path.join(dir, 'ids.list');
    if (c.baseline !== null) writeFileSync(file, c.baseline);
    let out = '';
    const code = await run(
      { mode: c.mode, baseline: 'ids.list', cwd: dir, env: { ...(c.env ?? {}) } },
      {
        lines: linesOf(c.input),
        write: (text: string): void => {
          out += text;
        },
      },
    );
    const problems: string[] = [];
    if (code !== c.expect) problems.push(`exit ${code}, expected ${c.expect}`);
    if (c.says !== undefined && !out.includes(c.says))
      problems.push(`no output holding "${c.says}"`);
    if (c.unchanged === true && c.baseline !== null && readFileSync(file, 'utf8') !== c.baseline) {
      problems.push('the baseline was rewritten');
    }
    return problems.length === 0 ? null : `${c.name}: ${problems.join('; ')}`;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

const cases = [...planted, ...clean];
const failures = (await Promise.all(cases.map(runCase))).filter((line) => line !== null);
for (const failure of failures)
  process.stdout.write(`test identity self-test: not as expected: ${failure}\n`);
const expected = cases.length - failures.length;
process.stdout.write(`test identity self-test: cases ${cases.length}, as expected ${expected}\n`);
process.exitCode = cases.length > 0 && expected === cases.length ? 0 : 1;
