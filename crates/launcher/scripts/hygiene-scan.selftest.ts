// Self-test of the tracked-file scan: scratch repositories holding the file
// samples of hygiene-fixtures.txt, scanned through the scan's command line.
// Each planted sample must be reported with exactly its rules, each clean one
// must pass, each exempt path must suppress only its own rules and print the
// count, and no output may repeat a planted string or the identity entry.
// Copies of the two helpers beside a stub audit script show that the path
// shapes come only from that script.
//
//   node crates/launcher/scripts/hygiene-scan.selftest.ts
import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  all,
  childEnv,
  expectsOf,
  git,
  judgeHits,
  newTally,
  one,
  outputProblems,
  readFixtures,
  record,
} from './commit-check.selftest.ts';
import type { Block, HitLine, Ran, Tally } from './commit-check.selftest.ts';

const HERE = import.meta.dirname;
const SCAN = path.join(HERE, 'hygiene-scan.ts');
const AUDIT = path.resolve(HERE, '..', '..', '..', 'scripts', 'binary-audit.sh');
const MAX_BUFFER = 64 * 1024 * 1024;

interface TreeFile {
  readonly text: string;
  readonly exec: boolean;
}

type Tree = ReadonlyMap<string, TreeFile>;

// What every scratch repository holds: the three exempt paths (a missing one
// fails the scan), a tracked script and the line-ending attributes.
const DEFAULT_TREE: Tree = new Map([
  ['.gitattributes', { text: '* text=auto eol=lf\n', exec: false }],
  ['README.md', { text: 'A sample repository.\n', exec: false }],
  ['scripts/sample.sh', { text: '#!/usr/bin/env bash\necho sample\n', exec: true }],
  ['scripts/binary-audit.sh', { text: '#!/usr/bin/env bash\necho placeholder\n', exec: true }],
  ['crates/launcher/scripts/hygiene-fixtures.txt', { text: 'placeholder\n', exec: false }],
  ['crates/launcher/package-lock.json', { text: '{}\n', exec: false }],
]);

interface Context {
  readonly root: string;
  readonly blocks: readonly Block[];
  readonly needles: readonly string[];
  readonly entry: string;
  readonly tally: Tally;
}

// Writes the files and adds them to the index of a new repository; the scan
// reads the index, so nothing is committed. Every *.sh is executable on disk,
// so a tracked mode can differ from the filesystem's.
const makeTree = (c: Context, name: string, files: Tree): string => {
  const dir = path.join(c.root, name);
  mkdirSync(dir, { recursive: true });
  git(dir, ['init', '-q', '-b', 'main']);
  for (const [file, content] of files) {
    mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
    writeFileSync(path.join(dir, file), content.text);
    if (file.endsWith('.sh')) chmodSync(path.join(dir, file), 0o755);
    git(dir, ['add', '--', file]);
    git(dir, ['update-index', `--chmod=${content.exec ? '+x' : '-x'}`, '--', file]);
  }
  return dir;
};

const withFiles = (base: Tree, extra: readonly (readonly [string, TreeFile])[]): Tree =>
  new Map([...base, ...extra]);

const runScan = (
  cwd: string,
  args: readonly string[],
  env: Readonly<Record<string, string>>,
  scan = SCAN,
): Ran => {
  const result = spawnSync(process.execPath, [scan, ...args], {
    cwd,
    encoding: 'utf8',
    maxBuffer: MAX_BUFFER,
    env: childEnv(env),
  });
  return { status: result.status ?? -1, out: `${result.stdout}${result.stderr}` };
};

// "<file>:<line>: error: <rule> -- <detail>" lines, as hits located by file.
const scanHits = (out: string): HitLine[] =>
  out.split('\n').flatMap((line) => {
    const found = /^(.+?):([0-9]+): error: (.+?)(?: -- .*)?$/u.exec(line);
    return found === null ? [] : [{ location: found[1] ?? '', rule: found[3] ?? '', line }];
  });

const lineOf = (out: string, prefix: string): string =>
  out.split('\n').find((line) => line.startsWith(prefix)) ?? '';

const checkOutput = (c: Context, name: string, ran: Ran): void => {
  for (const problem of outputProblems(ran.out, c.needles, c.entry))
    c.tally.problems.push(`${name}: ${problem}`);
};

// A scan that must fail and print a line holding the reason.
const expectRefusal = (c: Context, name: string, ran: Ran, reason: string): void => {
  checkOutput(c, name, ran);
  const problems = [
    ...(ran.status === 1 ? [] : [`exit ${String(ran.status)}, not 1`]),
    ...(ran.out.includes(reason) ? [] : [`no line holding "${reason}"`]),
  ];
  record(c.tally, true, name, problems.length === 0 ? null : problems.join('; '));
};

// The text path shapes of the real audit script, counted here, not read
// from the scan.
const realShapeCount = (): number => {
  const result = spawnSync('bash', [AUDIT, 'shapes'], { encoding: 'utf8' });
  return result.stdout
    .split('\n')
    .map((line) => line.split('\t'))
    .filter((fields) => fields[1] === 'path' && fields[3] === 'binary+text').length;
};

const textFileCount = (dir: string): number =>
  git(dir, ['grep', '-I', '-l', ''])
    .split('\n')
    .filter((line) => line !== '').length;

const fileBlocks = (c: Context): Block[] => c.blocks.filter((block) => block.kind === 'file');

const trapFiles = (c: Context): [string, TreeFile][] =>
  c.blocks
    .filter((block) => block.kind === 'trap')
    .map((block) => [`traps/${block.name}.txt`, { text: `${block.text}\n`, exec: false }]);

// Every file sample and trap in one repository, scanned with the identity
// entry set: each block is judged by the hits at its path.
const planted = (c: Context): void => {
  const blocks = fileBlocks(c);
  const tree = withFiles(DEFAULT_TREE, [
    ...blocks.map((block): [string, TreeFile] => [
      one(block, 'path') ?? `notes/${block.name}.txt`,
      { text: `${block.text}\n`, exec: one(block, 'exec') === 'yes' },
    ]),
    ...trapFiles(c),
  ]);
  const dir = makeTree(c, 'files', tree);
  const ran = runScan(dir, ['scan'], { CI_CHECK_IDENTITY: c.entry });
  checkOutput(c, 'the file samples', ran);
  const hits = scanHits(ran.out);
  for (const block of blocks) {
    const file = one(block, 'path') ?? '';
    const want = expectsOf(all(block, 'expect'));
    const suppressed = one(block, 'suppressed');
    const problems = [
      judgeHits(
        want,
        hits.filter((hit) => hit.location === file),
      ),
    ].filter((why) => why !== null);
    if (
      suppressed !== undefined &&
      !lineOf(ran.out, `exempt ${file} from `).endsWith(`: ${suppressed} suppressed`)
    )
      problems.push(`the exempt line does not count ${suppressed} suppressed`);
    // The mode sample is executable on disk: the scan must read the index.
    if (
      want.some((expect) => expect.rule === 'script mode') &&
      (statSync(path.join(dir, file)).mode & 0o100) === 0
    )
      problems.push('the sample is not executable on disk');
    record(
      c.tally,
      want.length > 0 || suppressed !== undefined,
      `file ${block.name}`,
      problems.length === 0 ? null : problems.join('; '),
    );
  }
  for (const [file] of trapFiles(c)) {
    const count = hits.filter((hit) => hit.location === file).length;
    record(c.tally, false, `the trap ${file}`, count === 0 ? null : `${String(count)} hits`);
  }
  const counts = [
    `files scanned: ${String(textFileCount(dir))}`,
    `path shapes read: ${String(realShapeCount())}`,
  ];
  for (const line of counts) {
    if (!ran.out.split('\n').includes(line))
      c.tally.problems.push(`the file samples: no line "${line}"`);
  }
  if (ran.status !== 1)
    c.tally.problems.push(`the file samples: exit ${String(ran.status)}, not 1`);
  const quiet = runScan(dir, ['scan'], {});
  checkOutput(c, 'the file samples without entries', quiet);
  const identityHits = scanHits(quiet.out).filter((hit) => hit.rule === 'identity entry').length;
  const why = [
    ...(identityHits === 0 ? [] : [`${String(identityHits)} identity hits`]),
    ...(quiet.out.split('\n').includes('identity: absent') ? [] : ['no line "identity: absent"']),
  ];
  record(
    c.tally,
    false,
    'the file samples without entries',
    why.length === 0 ? null : why.join('; '),
  );
};

const clean = (c: Context): void => {
  const dir = makeTree(c, 'clean', DEFAULT_TREE);
  const ran = runScan(dir, ['scan'], { CI_CHECK_IDENTITY: c.entry });
  checkOutput(c, 'a clean repository', ran);
  const hits = scanHits(ran.out).length;
  const why = [
    ...(ran.status === 0 ? [] : [`exit ${String(ran.status)}`]),
    ...(hits === 0 ? [] : [`${String(hits)} hits`]),
    ...(lineOf(ran.out, 'hits: ') === '' ? ['no totals line'] : []),
  ];
  record(c.tally, false, 'a clean repository', why.length === 0 ? null : why.join('; '));
};

const refusals = (c: Context): void => {
  const stale = new Map(DEFAULT_TREE);
  stale.delete('crates/launcher/package-lock.json');
  expectRefusal(
    c,
    'an exempt path that is not tracked',
    runScan(makeTree(c, 'stale', stale), ['scan'], {}),
    'the exempt path crates/launcher/package-lock.json is not tracked',
  );
  const empty = path.join(c.root, 'empty');
  mkdirSync(empty);
  git(empty, ['init', '-q', '-b', 'main']);
  expectRefusal(c, 'an empty repository', runScan(empty, ['scan'], {}), 'files scanned: 0');
  const noScript = new Map(DEFAULT_TREE);
  noScript.delete('scripts/sample.sh');
  noScript.delete('scripts/binary-audit.sh');
  expectRefusal(
    c,
    'no tracked shell script',
    runScan(makeTree(c, 'no-script', noScript), ['scan'], {}),
    'no tracked *.sh file was found',
  );
};

// Line endings come from the attributes: without them every text file is a
// hit, and a pattern that asks for CRLF makes its files hits.
const lineEndings = (c: Context): void => {
  const bare = new Map(DEFAULT_TREE);
  bare.delete('.gitattributes');
  const dir = makeTree(c, 'no-attributes', bare);
  const ran = runScan(dir, ['scan'], {});
  const hits = scanHits(ran.out).filter((hit) => hit.rule === 'line endings');
  const files = textFileCount(dir);
  const why = [
    ...(hits.length === files
      ? []
      : [`${String(hits.length)} line-ending hits for ${String(files)} files`]),
    ...(ran.status === 1 ? [] : [`exit ${String(ran.status)}, not 1`]),
  ];
  record(
    c.tally,
    true,
    'a repository without line-ending attributes',
    why.length === 0 ? null : why.join('; '),
  );
  const crlf = withFiles(DEFAULT_TREE, [
    ['.gitattributes', { text: '* text=auto eol=lf\n*.bat eol=crlf\n', exec: false }],
    ['samples/run.bat', { text: 'echo sample\r\n', exec: false }],
  ]);
  const batch = runScan(makeTree(c, 'crlf-attribute', crlf), ['scan'], {});
  record(
    c.tally,
    true,
    'a pattern that asks for CRLF',
    judgeHits([{ rule: 'line endings', words: 'crlf' }], scanHits(batch.out)),
  );
};

// Both helpers copied verbatim beside a stub audit script: the scan applies
// whatever text shapes the script lists, and fails when it lists none.
const copiedHelpers = (c: Context): void => {
  const stubbed = (name: string, shapes: string): string => {
    const tree = path.join(c.root, name);
    mkdirSync(path.join(tree, 'crates', 'launcher', 'scripts'), { recursive: true });
    mkdirSync(path.join(tree, 'scripts'), { recursive: true });
    for (const file of ['commit-check.ts', 'hygiene-scan.ts'])
      copyFileSync(path.join(HERE, file), path.join(tree, 'crates', 'launcher', 'scripts', file));
    writeFileSync(path.join(tree, 'crates', 'launcher', 'package.json'), '{"type": "module"}\n');
    writeFileSync(
      path.join(tree, 'scripts', 'binary-audit.sh'),
      `#!/usr/bin/env bash\nif [ "$1" = shapes ]; then\n${shapes}  exit 0\nfi\nexec bash '${AUDIT}' "$@"\n`,
    );
    return path.join(tree, 'crates', 'launcher', 'scripts', 'hygiene-scan.ts');
  };
  const planted = withFiles(DEFAULT_TREE, [
    ['notes/sample-root.txt', { text: 'Seen in /sample-root/x.\n', exec: false }],
  ]);
  const dir = makeTree(c, 'copied-target', planted);
  const none = runScan(dir, ['scan'], {}, stubbed('copied-none', ''));
  expectRefusal(c, 'a shape list without text path shapes', none, 'path shapes read: 0');
  const extra = runScan(
    dir,
    ['scan'],
    {},
    stubbed(
      'copied-extra',
      `  bash '${AUDIT}' shapes\n  printf 'path-sample\\tpath\\ti\\tbinary+text\\t/sample-root/\\n'\n`,
    ),
  );
  checkOutput(c, 'a shape list with one more text path shape', extra);
  const want = `path shapes read: ${String(realShapeCount() + 1)}`;
  const why = [
    judgeHits(
      [{ rule: 'local path', words: 'path-sample' }],
      scanHits(extra.out).filter((hit) => hit.location === 'notes/sample-root.txt'),
    ),
    extra.out.split('\n').includes(want) ? null : `no line "${want}"`,
    extra.status === 1 ? null : `exit ${String(extra.status)}, not 1`,
  ].filter((item) => item !== null);
  record(
    c.tally,
    true,
    'a shape list with one more text path shape',
    why.length === 0 ? null : why.join('; '),
  );
};

// A stub tool first on PATH: it copies the file list it was handed to
// $STUB_RECORD, answers a listing pass with the first $STUB_FILES paths, and
// exits $STUB_EXIT otherwise.
const STUB_TOOL = [
  '#!/usr/bin/env bash',
  "list='' listing=no",
  ': >"$STUB_RECORD"',
  'while [ $# -gt 0 ]; do',
  '  case "$1" in',
  '  --file-list) list=$2; cat -- "$2" >>"$STUB_RECORD"; shift ;;',
  '  --files) listing=yes ;;',
  '  --format | -f) shift ;;',
  '  -*) ;;',
  '  *) printf \'%s\\n\' "$1" >>"$STUB_RECORD" ;;',
  '  esac',
  '  shift',
  'done',
  'if [ "$listing" = yes ]; then head -n "$STUB_FILES" -- "$list"; exit 0; fi',
  'exit "$STUB_EXIT"',
  '',
].join('\n');

interface Wrapped {
  readonly ran: Ran;
  // The file list the stub tool was handed, one path per line.
  readonly list: string[];
}

// Runs one wrapper mode in a scratch repository with the stub tool first on PATH.
const runWrapped = (
  c: Context,
  name: string,
  tool: string,
  dir: string,
  env: Readonly<Record<string, string>>,
): Wrapped => {
  const bin = path.join(c.root, `${name}-bin`);
  mkdirSync(bin, { recursive: true });
  writeFileSync(path.join(bin, tool), STUB_TOOL);
  chmodSync(path.join(bin, tool), 0o755);
  const list = path.join(c.root, `${name}-list.txt`);
  writeFileSync(list, '');
  const ran = runScan(dir, [tool], {
    PATH: `${bin}${path.delimiter}${process.env['PATH'] ?? ''}`,
    STUB_RECORD: list,
    STUB_FILES: '1000',
    STUB_EXIT: '0',
    ...env,
  });
  checkOutput(c, name, ran);
  return {
    ran,
    list: readFileSync(list, 'utf8')
      .split('\n')
      .filter((line) => line !== ''),
  };
};

const sameList = (a: readonly string[], b: readonly string[]): boolean =>
  [...a].sort().join('\n') === [...b].sort().join('\n');

const reasons = (items: readonly (string | null)[]): string | null => {
  const found = items.filter((item) => item !== null);
  return found.length === 0 ? null : found.join('; ');
};

// typos gets the scan's own text-file list: a listing pass for the count,
// then the check over the same list.
const typosWrapper = (c: Context): void => {
  const dir = makeTree(c, 'typos', DEFAULT_TREE);
  const files = git(dir, ['grep', '-I', '-l', ''])
    .split('\n')
    .filter((line) => line !== '');
  const three = runWrapped(c, 'typos-three', 'typos', dir, { STUB_FILES: '3' });
  record(
    c.tally,
    false,
    'typos over the text-file list',
    reasons([
      three.ran.status === 0 ? null : `exit ${String(three.ran.status)}`,
      three.ran.out.split('\n').includes('typos: files checked 3')
        ? null
        : 'no line "typos: files checked 3"',
      sameList(three.list, files) ? null : 'the list handed to typos is not the tracked text files',
    ]),
  );
  const failing = runWrapped(c, 'typos-failing', 'typos', dir, { STUB_EXIT: '2' });
  record(
    c.tally,
    true,
    'typos reporting a misspelling',
    failing.ran.status === 0 ? 'the wrapper passed' : null,
  );
  const none = runWrapped(c, 'typos-none', 'typos', dir, { STUB_FILES: '0' });
  record(
    c.tally,
    true,
    'typos checking 0 files',
    reasons([
      none.ran.status === 0 ? 'the wrapper passed' : null,
      none.ran.out.includes('typos: files checked 0') ? null : 'no line "typos: files checked 0"',
    ]),
  );
};

// shellcheck gets the tracked *.sh files only: an untracked script next to
// them is not handed over.
const shellcheckWrapper = (c: Context): void => {
  const dir = makeTree(c, 'shellcheck', DEFAULT_TREE);
  writeFileSync(path.join(dir, 'scripts', 'untracked.sh'), '#!/usr/bin/env bash\necho x\n');
  const scripts = git(dir, ['ls-files', '--', '*.sh'])
    .split('\n')
    .filter((line) => line !== '');
  const passing = runWrapped(c, 'shellcheck-passing', 'shellcheck', dir, {});
  record(
    c.tally,
    false,
    'shellcheck over the tracked scripts',
    reasons([
      passing.ran.status === 0 ? null : `exit ${String(passing.ran.status)}`,
      passing.ran.out.includes(`shellcheck: scripts ${String(scripts.length)}`)
        ? null
        : `no line "shellcheck: scripts ${String(scripts.length)}"`,
      sameList(passing.list, scripts)
        ? null
        : 'the list handed to shellcheck is not the tracked scripts',
      passing.list.includes('scripts/untracked.sh') ? 'the untracked script was handed over' : null,
    ]),
  );
  const failing = runWrapped(c, 'shellcheck-failing', 'shellcheck', dir, { STUB_EXIT: '1' });
  record(
    c.tally,
    true,
    'shellcheck reporting a finding',
    failing.ran.status === 0 ? 'the wrapper passed' : null,
  );
  const bare = new Map(DEFAULT_TREE);
  bare.delete('scripts/sample.sh');
  bare.delete('scripts/binary-audit.sh');
  const none = runWrapped(c, 'shellcheck-none', 'shellcheck', makeTree(c, 'no-scripts', bare), {});
  record(
    c.tally,
    true,
    'shellcheck with no tracked script',
    reasons([
      none.ran.status === 0 ? 'the wrapper passed' : null,
      none.ran.out.includes('shellcheck: scripts 0') ? null : 'no line "shellcheck: scripts 0"',
    ]),
  );
};

// A stub prettier at the launcher's by-path entry point: it records its
// arguments, one per line, and exits $STUB_EXIT.
const STUB_PRETTIER = [
  "const { writeFileSync } = require('node:fs');",
  "writeFileSync(process.env.STUB_RECORD, process.argv.slice(2).join('\\n') + '\\n');",
  'process.exitCode = Number(process.env.STUB_EXIT);',
  '',
].join('\n');

const PRETTIER_TREE = withFiles(DEFAULT_TREE, [
  ['crates/launcher/package.json', { text: '{}\n', exec: false }],
  ['crates/launcher/src-tauri/sample.json', { text: '{}\n', exec: false }],
  ['.github/sample.yml', { text: 'name: sample\n', exec: false }],
]);

const runPrettier = (c: Context, name: string, files: Tree, exit: string): Wrapped => {
  const dir = makeTree(c, name, files);
  const bin = path.join(dir, 'crates', 'launcher', 'node_modules', 'prettier', 'bin');
  mkdirSync(bin, { recursive: true });
  writeFileSync(path.join(bin, 'prettier.cjs'), STUB_PRETTIER);
  const list = path.join(c.root, `${name}-argv.txt`);
  writeFileSync(list, '');
  const ran = runScan(dir, ['prettier'], { STUB_RECORD: list, STUB_EXIT: exit });
  checkOutput(c, name, ran);
  return {
    ran,
    list: readFileSync(list, 'utf8')
      .split('\n')
      .filter((line) => line !== ''),
  };
};

// The root prettier check: the launcher's config, its ignore file off, and
// exactly the tracked markdown, YAML and JSON files but the two npm writes.
const prettierWrapper = (c: Context): void => {
  const passing = runPrettier(c, 'prettier-passing', PRETTIER_TREE, '0');
  const files = ['.github/sample.yml', 'README.md', 'crates/launcher/src-tauri/sample.json'];
  const argv = passing.list;
  const listed = argv.filter((arg) => arg.startsWith('../../'));
  record(
    c.tally,
    false,
    'prettier over the root text files',
    reasons([
      passing.ran.status === 0 ? null : `exit ${String(passing.ran.status)}`,
      passing.ran.out.includes(`root prettier: files ${String(files.length)}`)
        ? null
        : `no line "root prettier: files ${String(files.length)}"`,
      argv.includes('--check') ? null : 'no --check',
      argv.join(' ').includes('--config .prettierrc.json') ? null : 'no --config .prettierrc.json',
      argv.includes('--ignore-path=') ? null : 'the ignore file is not turned off',
      sameList(
        listed,
        files.map((file) => `../../${file}`),
      )
        ? null
        : 'the list is not the tracked root text files',
    ]),
  );
  const failing = runPrettier(c, 'prettier-failing', PRETTIER_TREE, '1');
  record(
    c.tally,
    true,
    'prettier reporting a file',
    failing.ran.status === 0 ? 'the wrapper passed' : null,
  );
  const bare = new Map(DEFAULT_TREE);
  bare.delete('README.md');
  const none = runPrettier(c, 'prettier-none', bare, '0');
  record(
    c.tally,
    true,
    'prettier with no root text file',
    reasons([
      none.ran.status === 0 ? 'the wrapper passed' : null,
      none.ran.out.includes('root prettier: files 0') ? null : 'no line "root prettier: files 0"',
    ]),
  );
};

const main = (): number => {
  const blocks = readFixtures();
  const c: Context = {
    root: mkdtempSync(path.join(tmpdir(), 'hygiene-scan-')),
    blocks,
    needles: blocks.flatMap((block) => all(block, 'needle')),
    entry: blocks.find((block) => block.kind === 'identity')?.text ?? '',
    tally: newTally(),
  };
  try {
    planted(c);
    clean(c);
    refusals(c);
    lineEndings(c);
    copiedHelpers(c);
    typosWrapper(c);
    shellcheckWrapper(c);
    prettierWrapper(c);
  } finally {
    rmSync(c.root, { recursive: true, force: true });
  }
  const t = c.tally;
  for (const problem of t.problems)
    process.stdout.write(`hygiene scan self-test: not as expected: ${problem}\n`);
  process.stdout.write(
    `hygiene scan self-test: planted ${String(t.planted)} caught ${String(t.caught)}, clean ${String(t.clean)} passed ${String(t.passed)}\n`,
  );
  const ok =
    t.planted > 0 &&
    t.clean > 0 &&
    t.caught === t.planted &&
    t.passed === t.clean &&
    t.problems.length === 0;
  return ok ? 0 : 1;
};

process.exitCode = main();
