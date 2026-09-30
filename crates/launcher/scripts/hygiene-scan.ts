// Scans the repository's tracked text files for local paths (the text path
// shapes of scripts/binary-audit.sh), private email addresses, planning ids
// and, when CI_CHECK_IDENTITY is set, identity entries, which the audit
// script matches and never prints; every tracked path for a never-commit
// file; every tracked *.sh for mode 100755 in the index (never the
// filesystem's); every "$schema" value in a tracked JSON file for a relative
// path; and every text file for the eol attribute lf. Three paths are exempt
// from the first three rules by their exact path; every run prints what each
// of them suppressed, and an exempt path that is no longer tracked fails.
// The tool modes run a tool over a fixed list and pass its output through.
//
//   node crates/launcher/scripts/hygiene-scan.ts scan
//   node crates/launcher/scripts/hygiene-scan.ts typos
//   node crates/launcher/scripts/hygiene-scan.ts shellcheck
//   node crates/launcher/scripts/hygiene-scan.ts prettier
//
// typos: the scan's list of tracked text files, in a temporary file outside
// the repository; a listing pass prints the count, then the check runs over
// the same list. shellcheck: the tracked *.sh files, at .shellcheckrc's level.
// prettier: the tracked markdown, YAML and JSON files but the two npm writes,
// run from crates/launcher with its config (the config's plugin resolves only
// there) and without its ignore file, which would skip an explicit src-tauri/
// path and still report success.
//
// A hit line names the file, the line and the rule, never the matched text.
// Exit status: 0 pass, 1 a hit or a failed condition, 2 usage.
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  CheckFailure,
  isNeverCommitPath,
  matchIdentity,
  matchPathShapes,
  planningIdKinds,
  privateEmailCount,
  readPathShapes,
  repoRoot,
} from './commit-check.ts';

interface Line {
  readonly file: string;
  readonly no: number;
  readonly text: string;
}

interface ScanHit {
  readonly file: string;
  readonly line: number;
  readonly rule: string;
  readonly detail: string;
}

const MAX_BUFFER = 256 * 1024 * 1024;

const EXEMPT: readonly { readonly path: string; readonly rules: readonly string[] }[] = [
  // It holds the shape list and its self-test's planted paths.
  { path: 'scripts/binary-audit.sh', rules: ['local path'] },
  // Every planted sample of both self-tests.
  {
    path: 'crates/launcher/scripts/hygiene-fixtures.txt',
    rules: ['local path', 'private email', 'planning id'],
  },
  // npm writes base64 integrity values, in which an R and two digits can occur.
  { path: 'crates/launcher/package-lock.json', rules: ['planning id'] },
];

const SCAN_RULES = [
  'local path',
  'private email',
  'planning id',
  'never-commit file',
  'script mode',
  'schema reference',
  'line endings',
  'identity entry',
];

// git in the repository; exit 1 is allowed where git uses it for "none".
const git = (root: string, args: readonly string[], noneIsEmpty = false, input = ''): string => {
  const result = spawnSync('git', ['-c', 'i18n.logOutputEncoding=UTF-8', ...args], {
    cwd: root,
    input,
    encoding: 'utf8',
    maxBuffer: MAX_BUFFER,
    env: { ...process.env, LC_ALL: 'C.UTF-8' },
  });
  if (noneIsEmpty && result.status === 1 && result.stdout === '') return '';
  if (result.error !== undefined || result.status !== 0)
    throw new CheckFailure(`git ${args[0] ?? ''} exited ${String(result.status)}`);
  return result.stdout;
};

const nulList = (text: string): string[] => text.split('\0').filter((item) => item !== '');

// The tracked files git does not classify as binary.
const textFiles = (root: string): string[] =>
  nulList(git(root, ['grep', '-z', '-I', '-l', ''], true));

const linesOf = (root: string, file: string): Line[] => {
  const text = readFileSync(path.join(root, file), 'utf8');
  const lines = text.split('\n');
  if (lines.at(-1) === '') lines.pop();
  return lines.map((line, i) => ({ file, no: i + 1, text: line }));
};

const plural = (n: number, one: string, many: string): string =>
  `${String(n)} ${n === 1 ? one : many}`;

// Local paths, private emails, planning ids and identity entries, per line.
function contentHits(lines: readonly Line[]): {
  readonly hits: ScanHit[];
  readonly shapes: number;
  readonly identity: readonly string[];
} {
  const shapes = readPathShapes();
  const hits: ScanHit[] = [];
  const add = (line: Line, rule: string, detail: string): void => {
    hits.push({ file: line.file, line: line.no, rule, detail });
  };
  if (shapes.length > 0) {
    for (const found of matchPathShapes(
      shapes,
      lines.map((line) => line.text),
    )) {
      const line = lines[found.index];
      if (line !== undefined) add(line, 'local path', found.label);
    }
  }
  for (const line of lines) {
    const emails = privateEmailCount(line.text);
    if (emails > 0) add(line, 'private email', plural(emails, 'address', 'addresses'));
    const kinds = [...new Set(planningIdKinds(line.text))];
    if (kinds.length > 0) add(line, 'planning id', kinds.join(', '));
  }
  const byLocation = new Map(lines.map((line) => [`${line.file}:${String(line.no)}`, line]));
  const identity = matchIdentity(
    lines.map((line) => ({ location: `${line.file}:${String(line.no)}`, text: line.text })),
  );
  for (const found of identity.hits) {
    const line = byLocation.get(found.location);
    if (line !== undefined) add(line, 'identity entry', `entry ${String(found.index)}`);
  }
  return { hits, shapes: shapes.length, identity: identity.lines };
}

// Never-commit paths, script modes, $schema values and line endings.
function fileHits(
  root: string,
  tracked: readonly string[],
  files: readonly string[],
  lines: readonly Line[],
): { readonly hits: ScanHit[]; readonly scripts: number; readonly schemaKeys: number } {
  const hits: ScanHit[] = [];
  const add = (file: string, line: number, rule: string, detail: string): void => {
    hits.push({ file, line, rule, detail });
  };
  for (const file of tracked) if (isNeverCommitPath(file)) add(file, 1, 'never-commit file', '');
  // Modes come from the index: on a filesystem without modes every file
  // shows as executable.
  const scripts = nulList(git(root, ['ls-files', '--stage', '-z', '--', '*.sh']));
  for (const entry of scripts) {
    const found = /^([0-7]{6}) [0-9a-f]+ [0-3]\t(.+)$/su.exec(entry);
    if (found === null)
      throw new CheckFailure('git ls-files --stage printed an entry it could not read');
    const [, mode = '', file = ''] = found;
    if (mode !== '100755') add(file, 1, 'script mode', `${mode}, not 100755`);
  }
  let schemaKeys = 0;
  for (const line of lines.filter((item) => item.file.endsWith('.json'))) {
    for (const found of line.text.matchAll(/"\$schema"\s*:\s*"([^"]*)"/gu)) {
      schemaKeys += 1;
      if (/^[A-Za-z][A-Za-z0-9+.-]*:/u.test(found[1] ?? ''))
        add(line.file, line.no, 'schema reference', 'a URL, not a relative path');
    }
  }
  const input = files.map((file) => `${file}\0`).join('');
  const fields = git(root, ['check-attr', '-z', '--stdin', 'eol'], false, input).split('\0');
  if (fields.at(-1) === '') fields.pop();
  if (fields.length !== files.length * 3)
    throw new CheckFailure(
      `git check-attr answered ${String(fields.length / 3)} of ${String(files.length)} files`,
    );
  for (let i = 0; i < fields.length; i += 3) {
    const value = fields[i + 2] ?? '';
    if (value !== 'lf') add(fields[i] ?? '', 1, 'line endings', `eol is ${value}, not lf`);
  }
  return { hits, scripts: scripts.length, schemaKeys };
}

function scan(write: (line: string) => void): number {
  const root = repoRoot();
  const files = textFiles(root);
  const trackedList = nulList(git(root, ['ls-files', '-z']));
  const tracked = new Set(trackedList);
  write(`files scanned: ${String(files.length)}`);
  if (files.length === 0) {
    write('hygiene scan: FAIL no tracked text file was found');
    return 1;
  }
  const failures: string[] = [];
  for (const exempt of EXEMPT) {
    if (!tracked.has(exempt.path)) failures.push(`the exempt path ${exempt.path} is not tracked`);
  }
  const lines = files.flatMap((file) => linesOf(root, file));
  const content = contentHits(lines);
  write(`path shapes read: ${String(content.shapes)}`);
  if (content.shapes === 0) failures.push('scripts/binary-audit.sh listed 0 path shapes for text');
  const paths = fileHits(root, trackedList, files, lines);
  write(`shell scripts: ${String(paths.scripts)}`);
  write(`schema keys ${String(paths.schemaKeys)}`);
  if (paths.scripts === 0) failures.push('no tracked *.sh file was found');
  const all = [...content.hits, ...paths.hits];
  const exemptFrom = (hit: ScanHit): boolean =>
    EXEMPT.some((exempt) => exempt.path === hit.file && exempt.rules.includes(hit.rule));
  for (const exempt of EXEMPT) {
    const suppressed = all.filter((hit) => hit.file === exempt.path && exemptFrom(hit)).length;
    write(
      `exempt ${exempt.path} from ${exempt.rules.join(', ')}: ${String(suppressed)} suppressed`,
    );
  }
  const hits = all
    .filter((hit) => !exemptFrom(hit))
    .sort(
      (a, b) =>
        a.file.localeCompare(b.file) ||
        a.line - b.line ||
        SCAN_RULES.indexOf(a.rule) - SCAN_RULES.indexOf(b.rule),
    );
  for (const hit of hits) {
    const detail = hit.detail === '' ? '' : ` -- ${hit.detail}`;
    write(`${hit.file}:${String(hit.line)}: error: ${hit.rule}${detail}`);
  }
  write(
    `hits: ${SCAN_RULES.map((rule) => `${rule} ${String(hits.filter((hit) => hit.rule === rule).length)}`).join(', ')}`,
  );
  for (const line of content.identity) write(line);
  for (const failure of failures) write(`hygiene scan: FAIL ${failure}`);
  return hits.length === 0 && failures.length === 0 ? 0 : 1;
}

// ---------------------------------------------------------------------------
// The tool modes.

interface Ran {
  readonly status: number;
  readonly stdout: string;
  readonly stderr: string;
}

const run = (command: string, args: readonly string[], cwd: string): Ran => {
  const result = spawnSync(command, args, {
    cwd,
    encoding: 'utf8',
    maxBuffer: MAX_BUFFER,
    env: { ...process.env, LC_ALL: 'C.UTF-8' },
  });
  if (result.error !== undefined)
    throw new CheckFailure(`${command} could not be run (${result.error.message})`);
  return { status: result.status ?? 128, stdout: result.stdout, stderr: result.stderr };
};

// The tool's own output, unchanged, and its exit status.
const passThrough = (ran: Ran): number => {
  process.stdout.write(ran.stdout);
  process.stderr.write(ran.stderr);
  return ran.status;
};

// Writes the list to a file in a new temporary directory outside the
// repository, hands its path to use and removes the directory after.
const withListFile = (files: readonly string[], use: (list: string) => number): number => {
  const dir = mkdtempSync(path.join(tmpdir(), 'hygiene-list-'));
  try {
    const list = path.join(dir, 'files.txt');
    writeFileSync(list, files.map((file) => `${file}\n`).join(''));
    return use(list);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

function typos(write: (line: string) => void): number {
  const root = repoRoot();
  const files = textFiles(root);
  return withListFile(files, (list) => {
    const listed = run('typos', ['--file-list', list, '--files'], root);
    if (listed.status !== 0) {
      process.stderr.write(listed.stderr);
      write(`typos: FAIL the listing pass exited ${String(listed.status)}`);
      return 1;
    }
    const count = listed.stdout.split('\n').filter((line) => line !== '').length;
    write(`typos: files checked ${String(count)}`);
    if (count === 0) {
      write(`typos: FAIL typos listed 0 of the ${String(files.length)} tracked text files`);
      return 1;
    }
    return passThrough(run('typos', ['--file-list', list, '--format', 'brief'], root));
  });
}

function shellcheck(write: (line: string) => void): number {
  const root = repoRoot();
  const scripts = nulList(git(root, ['ls-files', '-z', '--', '*.sh']));
  write(`shellcheck: scripts ${String(scripts.length)}`);
  if (scripts.length === 0) {
    write('shellcheck: FAIL no tracked *.sh file was found');
    return 1;
  }
  return passThrough(run('shellcheck', ['-f', 'gcc', '--', ...scripts], root));
}

// npm writes these two; the launcher's own prettier row leaves them out too.
const PRETTIER_SKIPPED = ['crates/launcher/package.json', 'crates/launcher/package-lock.json'];

function prettier(write: (line: string) => void): number {
  const root = repoRoot();
  const files = nulList(git(root, ['ls-files', '-z'])).filter(
    (file) => /\.(?:md|ya?ml|json)$/u.test(file) && !PRETTIER_SKIPPED.includes(file),
  );
  write(`root prettier: files ${String(files.length)}`);
  if (files.length === 0) {
    write('root prettier: FAIL no tracked markdown, YAML or JSON file was found');
    return 1;
  }
  const args = ['--check', '--config', '.prettierrc.json', '--ignore-path='];
  return passThrough(
    run(
      process.execPath,
      ['node_modules/prettier/bin/prettier.cjs', ...args, ...files.map((file) => `../../${file}`)],
      path.join(root, 'crates', 'launcher'),
    ),
  );
}

const MODES: Readonly<Record<string, (write: (line: string) => void) => number>> = {
  scan,
  typos,
  shellcheck,
  prettier,
};

const USAGE = `usage: hygiene-scan.ts ${Object.keys(MODES).join(' | ')}`;

const main = (args: readonly string[]): number => {
  const write = (line: string): void => {
    process.stdout.write(`${line}\n`);
  };
  const [name = ''] = args;
  const mode = Object.hasOwn(MODES, name) ? MODES[name] : undefined;
  if (args.length !== 1 || mode === undefined) {
    process.stderr.write(`${USAGE}\n`);
    return 2;
  }
  try {
    return mode(write);
  } catch (error: unknown) {
    if (!(error instanceof CheckFailure)) throw error;
    const prefix: Readonly<Record<string, string>> = {
      scan: 'hygiene scan',
      prettier: 'root prettier',
    };
    write(`${prefix[name] ?? name}: FAIL ${error.message}`);
    return 1;
  }
};

process.exitCode = main(process.argv.slice(2));
