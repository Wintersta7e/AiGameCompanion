// Checks one test run's ids against its committed baseline.
//
//   node crates/launcher/scripts/test-ids.ts <libtest|libtest-ignored|tap> <baseline>
//
// The run's output arrives on stdin and is echoed line by line as it arrives.
// `libtest` and `tap` check the run against the baseline; `libtest-ignored`
// checks an `--ignored` run against the baseline's ignored ids. With
// CI_CHECK_UPDATE_BASELINES=1 (refused under GITHUB_ACTIONS) a passing run
// rewrites the baseline instead. On a pull_request run it also lists the ids
// changed since the base commit's copy of the baseline; that list never
// changes the verdict. Exit status: 0 pass, 1 fail, 2 usage or refusal.
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { createInterface } from 'node:readline';

export type Mode = 'libtest' | 'libtest-ignored' | 'tap';

export interface Entry {
  readonly id: string;
  readonly status: string;
}

export interface Parsed {
  readonly entries: readonly Entry[];
  readonly failedIds: readonly string[];
  // passed + ignored (libtest) or `# tests` (TAP); null when the run printed no summary.
  readonly summary: number | null;
  // failed (libtest) or fail + cancelled (TAP) from the summary lines.
  readonly summaryFailed: number;
  readonly filteredOut: number;
  readonly problems: readonly string[];
}

export type BaseRead =
  | { readonly state: 'present'; readonly text: string }
  | { readonly state: 'absent' }
  | { readonly state: 'unreadable'; readonly reason: string };

export interface RunOptions {
  readonly mode: Mode;
  readonly baseline: string;
  readonly cwd: string;
  readonly env: Readonly<Record<string, string | undefined>>;
}

export interface Io {
  readonly lines: AsyncIterable<string>;
  readonly write: (text: string) => void;
}

const byteOrder = (a: string, b: string): number =>
  Buffer.compare(Buffer.from(a, 'utf8'), Buffer.from(b, 'utf8'));

const lineOf = (entry: Entry): string => `${entry.id} ${entry.status}`;

const summed = (line: string, pattern: RegExp): number => {
  const found = pattern.exec(line);
  return found === null ? 0 : Number.parseInt(found[1] ?? '0', 10);
};

export function parseLibtest(text: string): Parsed {
  const entries: Entry[] = [];
  const failedIds: string[] = [];
  const problems: string[] = [];
  let passed = 0;
  let failed = 0;
  let ignored = 0;
  let filteredOut = 0;
  let sawSummary = false;
  for (const line of text.replaceAll('\r', '').split('\n')) {
    if (line.startsWith('test result:')) {
      sawSummary = true;
      passed += summed(line, /(\d+) passed/u);
      failed += summed(line, /(\d+) failed/u);
      ignored += summed(line, /(\d+) ignored/u);
      filteredOut += summed(line, /(\d+) filtered out/u);
      continue;
    }
    const cut = line.startsWith('test ') ? line.indexOf(' ... ', 5) : -1;
    if (cut < 0) continue;
    const id = line.slice(5, cut);
    const result = line.slice(cut + 5);
    if (result === 'ok') entries.push({ id, status: 'run' });
    else if (result === 'ignored' || result.startsWith('ignored, '))
      entries.push({ id, status: 'ignored' });
    else if (result === 'FAILED') failedIds.push(id);
    else problems.push(`unrecognised result "${result}" for ${id}`);
  }
  return {
    entries,
    failedIds,
    summary: sawSummary ? passed + ignored : null,
    summaryFailed: failed,
    filteredOut,
    problems,
  };
}

// The `type:` value of the YAML block that follows a TAP result line.
const yamlType = (lines: readonly string[], start: number): string | null => {
  if (lines[start]?.trim() !== '---') return null;
  for (let i = start + 1; i < lines.length; i += 1) {
    const body = (lines[i] ?? '').trim();
    if (body === '...') return null;
    const found = /^type: '([a-z]+)'$/u.exec(body);
    if (found !== null) return found[1] ?? null;
  }
  return null;
};

const tapStatus = (name: string): string => {
  const directive = / # (SKIP|TODO)(?: .*)?$/u.exec(name);
  if (directive === null) return 'run';
  return directive[1] === 'SKIP' ? 'skip' : 'todo';
};

export function parseTap(text: string): Parsed {
  const lines = text.replaceAll('\r', '').split('\n');
  const stack: string[] = [];
  const entries: Entry[] = [];
  const failedIds: string[] = [];
  let tests: number | null = null;
  let failed = 0;
  lines.forEach((line, index) => {
    const body = line.trimStart();
    const depth = Math.floor((line.length - body.length) / 4);
    const subtest = /^# Subtest: (.*)$/u.exec(body);
    if (subtest !== null) {
      stack.length = depth;
      stack[depth] = subtest[1] ?? '';
      return;
    }
    const result = /^(not ok|ok) \d+ - (.*)$/u.exec(body);
    if (result !== null) {
      if (yamlType(lines, index + 1) !== 'test') return;
      const id = stack.slice(0, depth + 1).join(' > ');
      if (result[1] === 'ok') entries.push({ id, status: tapStatus(result[2] ?? '') });
      else failedIds.push(id);
      return;
    }
    if (depth !== 0 || line !== body) return;
    if (/^# tests \d+$/u.test(body)) tests = summed(body, /(\d+)/u);
    else if (/^# (fail|cancelled) \d+$/u.test(body)) failed += summed(body, /(\d+)/u);
  });
  return {
    entries,
    failedIds,
    summary: tests,
    summaryFailed: failed,
    filteredOut: 0,
    problems: [],
  };
}

export function canonical(entries: readonly Entry[]): string {
  const lines = entries.map(lineOf).sort(byteOrder);
  return lines.length === 0 ? '' : `${lines.join('\n')}\n`;
}

// The baseline's entries: the status is the text after the last space.
const baselineEntries = (text: string): Entry[] =>
  text
    .split('\n')
    .filter((line) => line !== '')
    .map((line) => {
      const cut = line.lastIndexOf(' ');
      return cut < 0
        ? { id: line, status: '' }
        : { id: line.slice(0, cut), status: line.slice(cut + 1) };
    });

const isCanonical = (text: string): boolean => {
  const entries = baselineEntries(text);
  const unique = new Set(entries.map((entry) => entry.id)).size === entries.length;
  const shaped = entries.every((entry) => entry.status !== '' && !entry.id.startsWith('#'));
  return unique && shaped && canonical(entries) === text;
};

// The base commit's copy of the baseline: HEAD^1 of the checkout, which on a
// pull_request run is the merge commit's first parent.
export function readBase(file: string, cwd: string): BaseRead {
  const git = (args: readonly string[]): string =>
    execFileSync('git', args, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
  try {
    git(['rev-parse', '--verify', '--quiet', 'HEAD^1^{commit}']);
  } catch {
    return { state: 'unreadable', reason: 'HEAD^1 does not resolve to a commit' };
  }
  try {
    if (git(['ls-tree', 'HEAD^1', '--', file]).trim() === '') return { state: 'absent' };
    return { state: 'present', text: git(['show', `HEAD^1:${file}`]).replaceAll('\r', '') };
  } catch {
    return { state: 'unreadable', reason: `git could not read ${file} at HEAD^1` };
  }
}

interface Change {
  readonly added: readonly string[];
  readonly removed: readonly string[];
  readonly restatused: readonly string[];
}

const compare = (before: readonly Entry[], after: readonly Entry[]): Change => {
  const was = new Map(before.map((entry) => [entry.id, entry.status]));
  const now = new Map(after.map((entry) => [entry.id, entry.status]));
  const added = after.filter((entry) => !was.has(entry.id)).map(lineOf);
  const removed = before.filter((entry) => !now.has(entry.id)).map(lineOf);
  const restatused = after
    .filter((entry) => was.has(entry.id) && was.get(entry.id) !== entry.status)
    .map((entry) => `${entry.id}: ${was.get(entry.id) ?? ''} -> ${entry.status}`);
  return { added: added.sort(byteOrder), removed: removed.sort(byteOrder), restatused };
};

const changeCount = (change: Change): string =>
  `added ${change.added.length}, removed ${change.removed.length}, re-statused ${change.restatused.length}`;

const changeLines = (change: Change): string =>
  [
    ...change.added.map((line) => `  added: ${line}\n`),
    ...change.removed.map((line) => `  removed: ${line}\n`),
    ...change.restatused.map((line) => `  re-statused: ${line}\n`),
  ].join('');

// Lines to apply to turn the baseline into this run: - then + in byte order.
const unifiedDiff = (baseline: string, before: string, after: string): string => {
  const was = new Set(before.split('\n').filter((line) => line !== ''));
  const now = new Set(after.split('\n').filter((line) => line !== ''));
  const lines = [
    ...[...was].filter((line) => !now.has(line)).map((line) => `-${line}`),
    ...[...now].filter((line) => !was.has(line)).map((line) => `+${line}`),
  ].sort((a, b) => {
    const byId = byteOrder(a.slice(1), b.slice(1));
    return byId !== 0 ? byId : byteOrder(a, b);
  });
  return `--- a/${baseline}\n+++ b/${baseline}\n${lines.map((line) => `${line}\n`).join('')}`;
};

const duplicates = (entries: readonly Entry[]): string[] => {
  const seen = new Set<string>();
  const twice = new Set<string>();
  for (const entry of entries) {
    if (seen.has(entry.id)) twice.add(entry.id);
    seen.add(entry.id);
  }
  return [...twice].sort(byteOrder);
};

// Conditions (a) to (c) and the run's own failures, in words.
const parseProblems = (mode: Mode, parsed: Parsed): string[] => {
  const problems = [...parsed.problems];
  const ids = parsed.entries.length;
  const twice = duplicates(parsed.entries);
  if (ids === 0) problems.push('the run printed no test id');
  if (twice.length > 0) problems.push(`${twice.length} ids occur twice: ${twice.join(', ')}`);
  if (parsed.failedIds.length > 0) {
    problems.push(`${parsed.failedIds.length} tests failed: ${parsed.failedIds.join(', ')}`);
  }
  if (parsed.summary === null) problems.push('the run printed no summary line');
  else if (mode !== 'libtest-ignored' && parsed.summary !== ids) {
    problems.push(`the summary counts ${parsed.summary} tests but ${ids} ids were parsed`);
  }
  if (parsed.summaryFailed !== 0)
    problems.push(`the summary counts ${parsed.summaryFailed} failed tests`);
  if (mode === 'libtest' && parsed.filteredOut !== 0) {
    problems.push(`the summary counts ${parsed.filteredOut} tests filtered out`);
  }
  return problems;
};

const countLine = (mode: Mode, baseline: string, parsed: Parsed): string => {
  const count = (status: string): number =>
    parsed.entries.filter((entry) => entry.status === status).length;
  const statuses =
    mode === 'tap'
      ? `run ${count('run')} skip ${count('skip')} todo ${count('todo')}`
      : `run ${count('run')} ignored ${count('ignored')}`;
  return `test identity: ${baseline} ids ${parsed.entries.length} summary ${parsed.summary ?? 'none'} ${statuses}\n`;
};

// Absent is an empty set; any other read failure is reported, never guessed.
const readBaseline = (file: string): { readonly text: string } | { readonly error: string } => {
  try {
    return { text: readFileSync(file, 'utf8').replaceAll('\r', '') };
  } catch (error: unknown) {
    const code = (error as { code?: unknown }).code;
    return code === 'ENOENT' ? { text: '' } : { error: `cannot read it (${String(code)})` };
  }
};

const shortBase = (cwd: string): string => {
  try {
    return execFileSync('git', ['rev-parse', '--short', 'HEAD^1'], {
      cwd,
      encoding: 'utf8',
    }).trim();
  } catch {
    return 'HEAD^1';
  }
};

const disclose = (opts: RunOptions, entries: readonly Entry[], io: Io): boolean => {
  if (opts.env['GITHUB_EVENT_NAME'] !== 'pull_request') {
    io.write('test identity: no base comparison: not a pull_request run\n');
    return true;
  }
  const base = readBase(opts.baseline, opts.cwd);
  if (base.state === 'unreadable') {
    io.write(`test identity: FAIL the base commit's baseline is unreadable: ${base.reason}\n`);
    return false;
  }
  const short = shortBase(opts.cwd);
  if (base.state === 'absent')
    io.write(`test identity: the base commit ${short} has no ${opts.baseline}\n`);
  const change = compare(base.state === 'present' ? baselineEntries(base.text) : [], entries);
  io.write(`test identity: ids changed since the base commit ${short}: ${changeCount(change)}\n`);
  io.write(changeLines(change));
  return true;
};

const checkIgnored = (opts: RunOptions, parsed: Parsed, baseText: string, io: Io): boolean => {
  const wanted = baselineEntries(baseText)
    .filter((entry) => entry.status === 'ignored')
    .map((entry) => entry.id)
    .sort(byteOrder);
  const got = parsed.entries.map((entry) => entry.id).sort(byteOrder);
  const notOk = parsed.entries.filter((entry) => entry.status !== 'run').map((entry) => entry.id);
  let pass = true;
  if (wanted.length === 0) {
    io.write(`test identity: FAIL ${opts.baseline} lists no ignored test\n`);
    pass = false;
  }
  if (wanted.join('\n') !== got.join('\n')) {
    io.write(
      `test identity: FAIL the ignored run's ids differ from the ignored ids of ${opts.baseline}\n`,
    );
    io.write(`  expected: ${wanted.join(', ')}\n  ran: ${got.join(', ')}\n`);
    pass = false;
  }
  if (notOk.length > 0) {
    io.write(
      `test identity: FAIL ${notOk.length} ignored tests did not run: ${notOk.join(', ')}\n`,
    );
    pass = false;
  }
  return pass;
};

const update = (opts: RunOptions, parsed: Parsed, baseText: string, io: Io): number => {
  const text = canonical(parsed.entries);
  const change = compare(baselineEntries(baseText), parsed.entries);
  writeFileSync(path.resolve(opts.cwd, opts.baseline), text);
  io.write(
    `test identity: updated ${opts.baseline}: ${changeCount(change)}\n${changeLines(change)}`,
  );
  return 0;
};

const verdict = (opts: RunOptions, parsed: Parsed, baseText: string, io: Io): boolean => {
  let pass = true;
  for (const problem of parseProblems(opts.mode, parsed)) {
    io.write(`test identity: FAIL ${problem} (${opts.baseline})\n`);
    pass = false;
  }
  if (opts.mode === 'libtest-ignored') return checkIgnored(opts, parsed, baseText, io) && pass;
  if (!isCanonical(baseText)) {
    io.write(
      `test identity: FAIL ${opts.baseline} is not in canonical form (sorted, unique, one line per id)\n`,
    );
    pass = false;
  }
  const runText = canonical(parsed.entries);
  if (runText !== canonical(baselineEntries(baseText))) {
    io.write(`test identity: FAIL the run's ids differ from ${opts.baseline}:\n`);
    io.write(unifiedDiff(opts.baseline, canonical(baselineEntries(baseText)), runText));
    pass = false;
  }
  return disclose(opts, parsed.entries, io) && pass;
};

export async function run(opts: RunOptions, io: Io): Promise<number> {
  const updating = opts.env['CI_CHECK_UPDATE_BASELINES'] === '1' && opts.mode !== 'libtest-ignored';
  const chunks: string[] = [];
  for await (const line of io.lines) {
    io.write(`${line}\n`);
    chunks.push(line);
  }
  if (updating && opts.env['GITHUB_ACTIONS'] === 'true') {
    io.write(
      `test identity: CI_CHECK_UPDATE_BASELINES is refused under GITHUB_ACTIONS; ${opts.baseline} not written\n`,
    );
    return 2;
  }
  const text = chunks.join('\n');
  const parsed = opts.mode === 'tap' ? parseTap(text) : parseLibtest(text);
  const base = readBaseline(path.resolve(opts.cwd, opts.baseline));
  io.write(countLine(opts.mode, opts.baseline, parsed));
  if ('error' in base) {
    io.write(`test identity: FAIL ${opts.baseline}: ${base.error}\n`);
    return 1;
  }
  if (updating) {
    const problems = parseProblems(opts.mode, parsed);
    for (const problem of problems)
      io.write(`test identity: FAIL ${problem} (${opts.baseline} not written)\n`);
    return problems.length === 0 ? update(opts, parsed, base.text, io) : 1;
  }
  return verdict(opts, parsed, base.text, io) ? 0 : 1;
}

const isMode = (value: string | undefined): value is Mode =>
  value === 'libtest' || value === 'libtest-ignored' || value === 'tap';

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  const [mode, baseline] = process.argv.slice(2);
  if (!isMode(mode) || baseline === undefined || process.argv.length !== 4) {
    process.stderr.write('usage: test-ids.ts <libtest|libtest-ignored|tap> <baseline>\n');
    process.exitCode = 2;
  } else {
    const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
    const write = (text: string): void => {
      process.stdout.write(text);
    };
    process.exitCode = await run(
      { mode, baseline, cwd: process.cwd(), env: process.env },
      { lines, write },
    );
  }
}
