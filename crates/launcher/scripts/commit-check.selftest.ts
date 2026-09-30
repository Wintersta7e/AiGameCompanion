// Self-test of the commit checker: scratch repositories built from the samples
// in hygiene-fixtures.txt and checked through the checker's command line, as
// CI runs it (a synthetic event file) and as the local gate runs it (an origin
// URL and origin/main). Each planted sample must be reported once with exactly
// its rules, each clean one must pass, and no output may repeat a planted
// string or the identity entry.
//
//   node crates/launcher/scripts/commit-check.selftest.ts
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

export interface Block {
  readonly kind: string;
  readonly name: string;
  readonly keys: ReadonlyMap<string, readonly string[]>;
  readonly text: string;
}

export interface Expect {
  readonly rule: string;
  // Words the hit line must hold, or null.
  readonly words: string | null;
}

export interface HitLine {
  readonly location: string;
  readonly rule: string;
  readonly line: string;
}

export interface Ran {
  readonly status: number;
  readonly out: string;
}

const HERE = import.meta.dirname;
const CHECKER = path.join(HERE, 'commit-check.ts');
const MAX_BUFFER = 64 * 1024 * 1024;
const MAINTAINER = 'sample-owner@users.noreply.github.com';
const DROPPED_ENV = [
  'CI_CHECK_IDENTITY',
  'GITHUB_ACTIONS',
  'GITHUB_EVENT_NAME',
  'GITHUB_EVENT_PATH',
  'GITHUB_STEP_SUMMARY',
];

export const RULE_NAMES: readonly string[] = [
  'subject length',
  'body length',
  'AI attribution',
  'planning id',
  'test plan heading',
  'local path',
  'private email',
  'never-commit file',
  'co-author trailer',
  'noreply identity',
  'identity entry',
];

const expand = (text: string): string =>
  text.replaceAll(/\\u\{([0-9A-Fa-f]{1,6})\}/gu, (_whole, hex: string) =>
    String.fromCodePoint(Number.parseInt(hex, 16)),
  );

// Reads every block of the fixture file; a malformed line throws with its number.
export function readFixtures(): Block[] {
  const blocks: Block[] = [];
  let head: { kind: string; name: string } | null = null;
  let keys = new Map<string, string[]>();
  let text: string[] | null = null;
  const finish = (): void => {
    if (head === null) return;
    const body = [...(text ?? [])];
    while (body.at(-1) === '') body.pop();
    blocks.push({ kind: head.kind, name: head.name, keys, text: expand(body.join('\n')) });
  };
  const lines = readFileSync(path.join(HERE, 'hygiene-fixtures.txt'), 'utf8').split('\n');
  for (const [index, line] of lines.entries()) {
    if (line.startsWith('@@ ')) {
      finish();
      const [kind = '', name = ''] = line.slice(3).split(' ');
      head = { kind, name };
      keys = new Map<string, string[]>();
      text = null;
    } else if (head !== null && text !== null) {
      text.push(line);
    } else if (head !== null && line === 'text:') {
      text = [];
    } else if (head !== null && line !== '') {
      const found = /^([a-z-]+): (.*)$/u.exec(line);
      if (found === null)
        throw new Error(`hygiene-fixtures.txt line ${String(index + 1)} is not a key: value line`);
      const key = found[1] ?? '';
      keys.set(key, [...(keys.get(key) ?? []), expand(found[2] ?? '')]);
    }
  }
  finish();
  return blocks;
}

export const one = (block: Block, key: string): string | undefined => block.keys.get(key)?.[0];
export const all = (block: Block, key: string): readonly string[] => block.keys.get(key) ?? [];

export const expectsOf = (values: readonly string[]): Expect[] =>
  values.map((value) => {
    const cut = value.indexOf(' -- ');
    return cut < 0
      ? { rule: value, words: null }
      : { rule: value.slice(0, cut), words: value.slice(cut + 4) };
  });

// The environment of every child: no host git config, no identity entries and
// no CI variables unless the case sets them.
export const childEnv = (extra: Readonly<Record<string, string>>): NodeJS.ProcessEnv => ({
  ...Object.fromEntries(
    Object.entries(process.env).filter(([name]) => !DROPPED_ENV.includes(name)),
  ),
  GIT_CONFIG_GLOBAL: '/dev/null',
  GIT_CONFIG_NOSYSTEM: '1',
  LC_ALL: 'C.UTF-8',
  ...extra,
});

export const git = (
  dir: string,
  args: readonly string[],
  env: Readonly<Record<string, string>> = {},
  input = '',
): string => {
  const result = spawnSync('git', args, {
    cwd: dir,
    encoding: 'utf8',
    maxBuffer: MAX_BUFFER,
    env: childEnv(env),
    input,
  });
  if (result.status !== 0)
    throw new Error(
      `git ${args[0] ?? ''} exited ${String(result.status)}: ${result.stderr.trim()}`,
    );
  return result.stdout.trim();
};

let clock = 0;

export const commitTree = (
  dir: string,
  parents: readonly string[],
  message: string,
  author: string,
  committer: string,
): string => {
  clock += 60;
  const when = `@${String(1_767_225_600 + clock)} +0000`;
  return git(
    dir,
    ['commit-tree', git(dir, ['write-tree']), ...parents.flatMap((parent) => ['-p', parent])],
    {
      GIT_AUTHOR_NAME: 'Sample Author',
      GIT_AUTHOR_EMAIL: author,
      GIT_AUTHOR_DATE: when,
      GIT_COMMITTER_NAME: 'Sample Committer',
      GIT_COMMITTER_EMAIL: committer,
      GIT_COMMITTER_DATE: when,
    },
    message,
  );
};

export interface Built {
  readonly dir: string;
  readonly base: string;
  readonly head: string;
  // Fixture block name -> commit SHA.
  readonly shas: ReadonlyMap<string, string>;
}

// A repository whose base commit holds sample.txt, then one commit per block
// in order; a block with parents 2 is a merge whose second parent is the base.
export function buildRange(
  root: string,
  name: string,
  commits: readonly Block[],
  branch: string,
): Built {
  const dir = path.join(root, name);
  mkdirSync(dir, { recursive: true });
  git(dir, ['init', '-q', '-b', 'main']);
  writeFileSync(path.join(dir, 'sample.txt'), 'a sample file\n');
  git(dir, ['add', 'sample.txt']);
  const base = commitTree(
    dir,
    [],
    'chore: start the sample\n\nCo-Authored-By: Rooty\n',
    MAINTAINER,
    MAINTAINER,
  );
  let head = base;
  const shas = new Map<string, string>();
  for (const block of commits) {
    for (const file of (one(block, 'adds') ?? '').split(' ').filter((item) => item !== '')) {
      mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
      writeFileSync(path.join(dir, file), `${file}\n`);
      git(dir, ['add', '--', file]);
    }
    const rename = one(block, 'renames');
    if (rename !== undefined) {
      const [from = '', to = ''] = rename.split(' -> ');
      git(dir, ['mv', '--', from, to]);
    }
    const author = one(block, 'author') ?? MAINTAINER;
    head = commitTree(
      dir,
      one(block, 'parents') === '2' ? [head, base] : [head],
      `${block.text}\n`,
      author,
      one(block, 'committer') ?? author,
    );
    shas.set(block.name, head);
  }
  git(dir, ['update-ref', `refs/heads/${branch}`, head]);
  git(dir, ['symbolic-ref', 'HEAD', `refs/heads/${branch}`]);
  return { dir, base, head, shas };
}

export const runChecker = (
  cwd: string,
  args: readonly string[],
  env: Readonly<Record<string, string>>,
): Ran => {
  const result = spawnSync(process.execPath, [CHECKER, ...args], {
    cwd,
    encoding: 'utf8',
    maxBuffer: MAX_BUFFER,
    env: childEnv(env),
  });
  return { status: result.status ?? -1, out: `${result.stdout}${result.stderr}` };
};

export const hitLines = (out: string): HitLine[] =>
  out.split('\n').flatMap((line) => {
    const found =
      /^([0-9a-f]{12}|branch|pr-title|pr-body:[0-9]+)(?: author email| committer email)?: ([^:]+?)(?:: .*)?$/u.exec(
        line,
      );
    const rule = found?.[2] ?? '';
    return found !== null && RULE_NAMES.includes(rule)
      ? [{ location: found[1] ?? '', rule, line }]
      : [];
  });

// Why a sample's hit lines differ from its expect lines, or null.
export const judgeHits = (want: readonly Expect[], got: readonly HitLine[]): string | null => {
  const problems: string[] = [];
  for (const expect of want) {
    const matching = got.filter((hit) => hit.rule === expect.rule);
    const [first] = matching;
    if (matching.length !== 1 || first === undefined)
      problems.push(`${expect.rule} reported ${String(matching.length)} times, not once`);
    else if (expect.words !== null && !first.line.includes(expect.words))
      problems.push(`the ${expect.rule} hit does not say "${expect.words}"`);
  }
  for (const hit of got) {
    if (!want.some((expect) => expect.rule === hit.rule))
      problems.push(`an unexpected ${hit.rule} hit`);
  }
  return problems.length === 0 ? null : problems.join('; ');
};

// The self-test's own shapes: a rule id of the rule tables, and planning ids.
const RULE_ID = /\b[CH][0-9]{1,2}\b/u;
const PLANNING_SHAPES = [
  /(?<![A-Za-z0-9_])(?:P[0-9]+A[0-9]+|AC-P[0-9]+|p[0-9]+-[A-Za-z0-9_]+|[RUDV][0-9]{1,2})(?![A-Za-z0-9_])/u,
  /\xa7 ?[0-9]|docs\/(?:route|plans)\//u,
];

// What a captured output must never hold.
export const outputProblems = (
  out: string,
  needles: readonly string[],
  entry: string,
): string[] => {
  const problems: string[] = [];
  const repeated = needles.filter((needle) => out.includes(needle)).length;
  if (repeated > 0) problems.push(`the output repeats ${String(repeated)} planted strings`);
  if (entry !== '' && out.toLowerCase().includes(entry.toLowerCase()))
    problems.push('the output holds the identity entry');
  if (RULE_ID.test(out)) problems.push('the output holds a rule id');
  if (PLANNING_SHAPES.some((shape) => shape.test(out)))
    problems.push('the output holds a planning id shape');
  return problems;
};

export interface Tally {
  planted: number;
  caught: number;
  clean: number;
  passed: number;
  readonly problems: string[];
}

export const newTally = (): Tally => ({ planted: 0, caught: 0, clean: 0, passed: 0, problems: [] });

// Records one planted or clean case; why is null when it behaved as expected.
export const record = (tally: Tally, planted: boolean, name: string, why: string | null): void => {
  if (planted) {
    tally.planted += 1;
    if (why === null) tally.caught += 1;
  } else {
    tally.clean += 1;
    if (why === null) tally.passed += 1;
  }
  if (why !== null) tally.problems.push(`${name}: ${why}`);
};

const HTTPS_ORIGIN = 'https://github.com/sample-owner/sample-repo.git';
// Joined at run time: the ssh user and host together read as an email address.
const SSH_HOST = ['git', 'github.com'].join('@');
const SSH_ORIGIN = `${SSH_HOST}:sample-owner/sample-repo.git`;
const SSH_URL_ORIGIN = `ssh://${SSH_HOST}/sample-owner/sample-repo.git`;
const OWNER = { repository: { owner: { login: 'sample-owner' } } };

const setOrigin = (built: Built, url: string, main: boolean): void => {
  git(built.dir, ['config', 'remote.origin.url', url]);
  if (main) git(built.dir, ['update-ref', 'refs/remotes/origin/main', built.base]);
  else git(built.dir, ['update-ref', '-d', 'refs/remotes/origin/main']);
};

const ciEnv = (event: string, file: string): Record<string, string> => ({
  GITHUB_ACTIONS: 'true',
  GITHUB_EVENT_NAME: event,
  GITHUB_EVENT_PATH: file,
});

// The count line the fixture's class keys give for a group.
const splitLine = (commits: readonly Block[]): string => {
  const count = (name: string): number =>
    commits.filter((block) => one(block, 'class') === name).length;
  return `commits in range: ${String(commits.length)} (dependabot ${String(count('dependabot'))}, github merges ${String(count('github-merge'))}, maintainer ${String(count('maintainer'))}, other ${String(count('other'))})`;
};

interface Context {
  readonly root: string;
  readonly blocks: readonly Block[];
  readonly needles: readonly string[];
  readonly entry: string;
  readonly tally: Tally;
}

const group = (c: Context, name: string): Block[] =>
  c.blocks.filter((block) => block.kind === 'commit' && one(block, 'range') === name);

const checkOutput = (c: Context, name: string, ran: Ran): void => {
  for (const problem of outputProblems(ran.out, c.needles, c.entry))
    c.tally.problems.push(`${name}: ${problem}`);
};

// A clean run: exit 0, no hit line, and the line it must print.
const expectPass = (c: Context, name: string, ran: Ran, line: string): void => {
  checkOutput(c, name, ran);
  const hits = hitLines(ran.out).length;
  const problems = [
    ...(ran.status === 0 ? [] : [`exit ${String(ran.status)}`]),
    ...(hits === 0 ? [] : [`${String(hits)} hit lines`]),
    ...(ran.out.split('\n').includes(line) ? [] : [`no line "${line}"`]),
  ];
  record(c.tally, false, name, problems.length === 0 ? null : problems.join('; '));
};

// A run that must fail closed: exit 1 and a line holding the reason.
const expectRefusal = (c: Context, name: string, ran: Ran, reason: string): void => {
  checkOutput(c, name, ran);
  const problems = [
    ...(ran.status === 1 ? [] : [`exit ${String(ran.status)}, not 1`]),
    ...(ran.out.includes(reason) ? [] : [`no line holding "${reason}"`]),
  ];
  record(c.tally, true, name, problems.length === 0 ? null : problems.join('; '));
};

// Judges every block of a range run by its expect lines, and the branch by
// the group's branch-expect lines.
const judgeRange = (
  c: Context,
  name: string,
  built: Built,
  blocks: readonly Block[],
  ran: Ran,
): void => {
  checkOutput(c, name, ran);
  const hits = hitLines(ran.out);
  // Without its count line the checker did not read the range, so no sample passes.
  const counted = ran.out.split('\n').some((line) => line.startsWith('commits in range: '));
  let wanted = 0;
  for (const block of blocks) {
    const sha = (built.shas.get(block.name) ?? '').slice(0, 12);
    const want = expectsOf(all(block, 'expect'));
    wanted += want.length;
    const why = counted
      ? judgeHits(
          want,
          hits.filter((hit) => hit.location === sha),
        )
      : 'the checker printed no count line';
    record(c.tally, want.length > 0, `${name}, ${block.name}`, why);
  }
  const branchWant = expectsOf(blocks.flatMap((block) => all(block, 'branch-expect')));
  wanted += branchWant.length;
  const branchHits = hits.filter((hit) => hit.location === 'branch');
  if (branchWant.length > 0 || branchHits.length > 0)
    record(
      c.tally,
      branchWant.length > 0,
      `${name}, the branch`,
      judgeHits(branchWant, branchHits),
    );
  const known = new Set(['branch', ...[...built.shas.values()].map((sha) => sha.slice(0, 12))]);
  const stray = hits.filter((hit) => !known.has(hit.location)).length;
  if (stray > 0) c.tally.problems.push(`${name}: ${String(stray)} hits at no known location`);
  const status = wanted > 0 ? 1 : 0;
  if (ran.status !== status)
    c.tally.problems.push(`${name}: exit ${String(ran.status)}, not ${String(status)}`);
};

const classSplit = (c: Context): void => {
  const commits = group(c, 'split');
  const built = buildRange(c.root, 'split', commits, 'sample-branch');
  const line = splitLine(commits);
  const pr = path.join(c.root, 'split-pr.json');
  writeFileSync(
    pr,
    JSON.stringify({
      pull_request: { base: { sha: built.base }, head: { sha: built.head } },
      ...OWNER,
    }),
  );
  expectPass(
    c,
    'the class split from a pull_request event',
    runChecker(built.dir, ['commits'], ciEnv('pull_request', pr)),
    line,
  );
  const push = path.join(c.root, 'split-push.json');
  writeFileSync(push, JSON.stringify({ before: built.base, after: built.head, ...OWNER }));
  expectPass(
    c,
    'the class split from a push event',
    runChecker(built.dir, ['commits'], ciEnv('push', push)),
    line,
  );
  for (const [form, url] of [
    ['https', HTTPS_ORIGIN],
    ['ssh', SSH_ORIGIN],
    ['ssh URL', SSH_URL_ORIGIN],
  ] as const) {
    setOrigin(built, url, true);
    expectPass(
      c,
      `the class split read locally with an ${form} origin`,
      runChecker(built.dir, ['commits'], {}),
      line,
    );
  }
};

const branchOf = (blocks: readonly Block[]): string =>
  blocks.map((block) => one(block, 'branch')).find((value) => value !== undefined) ??
  'sample-branch';

const localRange = (
  c: Context,
  name: string,
  blocks: readonly Block[],
  env: Readonly<Record<string, string>> = {},
): void => {
  const built = buildRange(c.root, name, blocks, branchOf(blocks));
  setOrigin(built, HTTPS_ORIGIN, true);
  judgeRange(c, name, built, blocks, runChecker(built.dir, ['commits'], env));
};

const traps = (c: Context): Block[] =>
  c.blocks
    .filter((block) => block.kind === 'trap')
    .map((block) => ({
      kind: 'commit',
      name: `trap ${block.name}`,
      keys: new Map<string, string[]>(),
      text: `fix: keep the sample list sorted\n\n${block.text}\n\nCo-Authored-By: Rooty`,
    }));

const identity = (c: Context): void => {
  const blocks = group(c, 'identity');
  localRange(c, 'identity', blocks, { CI_CHECK_IDENTITY: c.entry });
  const built = buildRange(c.root, 'identity-unset', blocks, branchOf(blocks));
  setOrigin(built, HTTPS_ORIGIN, true);
  expectPass(
    c,
    'the identity range without entries',
    runChecker(built.dir, ['commits'], {}),
    'identity: absent',
  );
};

const refusals = (c: Context): void => {
  const built = buildRange(c.root, 'closed', group(c, 'clean').slice(0, 1), 'sample-branch');
  const side = commitTree(
    built.dir,
    [built.base],
    'chore: a side commit\n',
    MAINTAINER,
    MAINTAINER,
  );
  const event = (name: string, data: unknown): string => {
    const file = path.join(c.root, `${name}.json`);
    writeFileSync(file, JSON.stringify(data));
    return file;
  };
  const pr = (base: string, head: string): unknown => ({
    pull_request: { base: { sha: base }, head: { sha: head } },
    ...OWNER,
  });
  const cases: readonly (readonly [string, Readonly<Record<string, string>>, string])[] = [
    [
      'a pull_request range of 0 commits',
      ciEnv('pull_request', event('empty', pr(built.head, built.head))),
      'the range holds 0 commits',
    ],
    [
      'a push whose before is all zeros',
      ciEnv('push', event('zeros', { before: '0'.repeat(40), after: built.head, ...OWNER })),
      '.before is all zeros',
    ],
    [
      'a push without before',
      ciEnv('push', event('no-before', { after: built.head, ...OWNER })),
      'the event file has no .before',
    ],
    [
      'a push whose before is not an ancestor',
      ciEnv('push', event('side', { before: side, after: built.head, ...OWNER })),
      'is not an ancestor of',
    ],
    [
      'a head the clone does not hold',
      ciEnv('pull_request', event('unknown', pr(built.base, 'f'.repeat(40)))),
      'is not a commit in this clone',
    ],
    [
      'an event file without the range',
      ciEnv('pull_request', event('no-range', { pull_request: {}, ...OWNER })),
      'the event file has no .pull_request.base.sha',
    ],
    [
      'an event file without the owner',
      ciEnv(
        'pull_request',
        event('no-owner', {
          pull_request: { base: { sha: built.base }, head: { sha: built.head } },
        }),
      ),
      'the event file has no .repository.owner.login',
    ],
    [
      'no event file',
      { GITHUB_ACTIONS: 'true', GITHUB_EVENT_NAME: 'pull_request' },
      'GITHUB_EVENT_PATH is not set',
    ],
    [
      'an unreadable event file',
      ciEnv('pull_request', path.join(c.root, 'missing.json')),
      'could not be read as JSON',
    ],
    ['a schedule event', ciEnv('schedule', event('schedule', OWNER)), 'not schedule'],
  ];
  for (const [name, env, reason] of cases)
    expectRefusal(c, name, runChecker(built.dir, ['commits'], env), reason);
  setOrigin(built, HTTPS_ORIGIN, false);
  expectRefusal(
    c,
    'a local run without origin/main',
    runChecker(built.dir, ['commits'], {}),
    'origin/main does not resolve to a commit',
  );
  setOrigin(built, 'file:///nowhere/sample-repo.git', true);
  expectRefusal(
    c,
    'an origin URL that names no owner',
    runChecker(built.dir, ['commits'], {}),
    "could not be read from origin's URL",
  );
  setOrigin(built, HTTPS_ORIGIN, true);
  git(built.dir, ['update-ref', '--no-deref', 'HEAD', built.head]);
  expectPass(
    c,
    'a detached HEAD',
    runChecker(built.dir, ['commits'], {}),
    'branch: not checked, because HEAD is detached',
  );
};

interface PrText {
  readonly title: string;
  readonly body: string | null;
  readonly branch: string;
  readonly login: string;
}

const PR_CLEAN: PrText = {
  title: 'fix: keep the sample list sorted',
  body: 'Sorts the sample list.',
  branch: 'sample-branch',
  login: 'sample-contributor',
};

// Runs the PR text mode on a synthetic event file, outside any repository.
const runPrText = (c: Context, name: string, pr: PrText, event = 'pull_request'): Ran => {
  const file = path.join(c.root, `pr-${name}.json`);
  writeFileSync(
    file,
    JSON.stringify({
      pull_request: {
        title: pr.title,
        body: pr.body,
        head: { ref: pr.branch },
        user: { login: pr.login },
      },
    }),
  );
  return runChecker(c.root, ['pr-text'], { ...ciEnv(event, file) });
};

const fieldOf = (hit: HitLine): string => {
  if (hit.location === 'pr-title') return 'title';
  return hit.location === 'branch' ? 'branch' : 'body';
};

// Each pr block fills one field; its hits must sit in that field, and only
// hits marked "not counted" (a Dependabot body) leave the exit status at 0.
const prBlocks = (c: Context): void => {
  for (const block of c.blocks.filter((item) => item.kind === 'pr')) {
    const field = one(block, 'field') ?? 'title';
    const login = one(block, 'login') ?? PR_CLEAN.login;
    const ran = runPrText(c, block.name, {
      title: field === 'title' ? block.text : PR_CLEAN.title,
      body: field === 'body' ? block.text : PR_CLEAN.body,
      branch: field === 'branch' ? block.text : PR_CLEAN.branch,
      login,
    });
    checkOutput(c, `PR text ${block.name}`, ran);
    const want = expectsOf(all(block, 'expect'));
    const counted = want.filter((expect) => !(expect.words ?? '').includes('not counted'));
    const hits = hitLines(ran.out);
    const inField = hits.filter((hit) => fieldOf(hit) === field);
    const exempt = login === 'dependabot[bot]' ? 'yes' : 'no';
    const status = counted.length > 0 ? 1 : 0;
    const problems = [
      ...[judgeHits(want, inField)].filter((why) => why !== null),
      ...(inField.length === hits.length ? [] : ['a hit in another field']),
      ...(ran.status === status ? [] : [`exit ${String(ran.status)}, not ${String(status)}`]),
      ...(ran.out.includes(`dependabot body exemption: ${exempt}`)
        ? []
        : [`no count line with the exemption ${exempt}`]),
    ];
    record(
      c.tally,
      want.length > 0,
      `PR text ${block.name}`,
      problems.length === 0 ? null : problems.join('; '),
    );
  }
  for (const block of c.blocks.filter((item) => item.kind === 'trap')) {
    const ran = runPrText(c, `trap-${block.name}`, { ...PR_CLEAN, body: block.text });
    expectPass(
      c,
      `PR text with the trap ${block.name} in the body`,
      ran,
      'pr text: fields read 3 (title 32 characters, body 1 line, branch); dependabot body exemption: no',
    );
  }
  expectPass(
    c,
    'PR text with a null body',
    runPrText(c, 'null-body', { ...PR_CLEAN, body: null }),
    'pr text: fields read 3 (title 32 characters, body 0 lines, branch); dependabot body exemption: no',
  );
  expectRefusal(
    c,
    'PR text with an empty title',
    runPrText(c, 'empty-title', { ...PR_CLEAN, title: '' }),
    'the title read from the event file is empty',
  );
  expectRefusal(c, 'PR text from a push event', runPrText(c, 'push', PR_CLEAN, 'push'), 'not push');
  const noBranch = path.join(c.root, 'pr-no-branch.json');
  writeFileSync(noBranch, JSON.stringify({ pull_request: { title: 'fix: a title', body: '' } }));
  expectRefusal(
    c,
    'PR text without the branch',
    runChecker(c.root, ['pr-text'], ciEnv('pull_request', noBranch)),
    'the event file has no .pull_request.head.ref',
  );
  expectRefusal(
    c,
    'PR text without an event file',
    runChecker(c.root, ['pr-text'], { GITHUB_ACTIONS: 'true', GITHUB_EVENT_NAME: 'pull_request' }),
    'GITHUB_EVENT_PATH is not set',
  );
};

// The PR text workflow hands the event to the checker through the event file
// only: no run: value, inline or block, may interpolate the event.
const workflowRuns = (c: Context): void => {
  const file = path.join(HERE, '..', '..', '..', '.github', 'workflows', 'pr-text.yml');
  const interpolation = ['$', '{{ github.event.'].join('');
  let lines: string[] = [];
  try {
    lines = readFileSync(file, 'utf8').split('\n');
  } catch {
    c.tally.problems.push('the PR text workflow could not be read');
  }
  const runs: string[] = [];
  for (const [index, line] of lines.entries()) {
    const found = /^(\s*)(?:- )?run:\s*(.*)$/u.exec(line);
    if (found === null) continue;
    const indent = (found[1] ?? '').length;
    const value = found[2] ?? '';
    if (!/^[|>]/u.test(value)) {
      runs.push(value);
      continue;
    }
    const body: string[] = [];
    for (const next of lines.slice(index + 1)) {
      if (next.trim() !== '' && next.length - next.trimStart().length <= indent) break;
      body.push(next);
    }
    runs.push(body.join('\n'));
  }
  const interpolated = runs.filter((value) => value.includes(interpolation)).length;
  process.stdout.write(`workflow run blocks checked ${String(runs.length)}\n`);
  let why: string | null = null;
  if (runs.length === 0) why = 'no run: value was found';
  else if (interpolated > 0) why = `${String(interpolated)} run: values interpolate the event`;
  record(c.tally, false, 'the PR text workflow', why);
};

const main = (): number => {
  const blocks = readFixtures();
  const entry = blocks.find((block) => block.kind === 'identity')?.text ?? '';
  const c: Context = {
    root: mkdtempSync(path.join(tmpdir(), 'commit-check-')),
    blocks,
    needles: blocks.flatMap((block) => all(block, 'needle')),
    entry,
    tally: newTally(),
  };
  try {
    classSplit(c);
    localRange(c, 'planted', group(c, 'planted'));
    localRange(c, 'clean', [...group(c, 'clean'), ...traps(c)]);
    localRange(c, 'branch-planning', group(c, 'branch-planning'));
    localRange(c, 'branch-path', group(c, 'branch-path'));
    identity(c);
    refusals(c);
    prBlocks(c);
    workflowRuns(c);
  } finally {
    rmSync(c.root, { recursive: true, force: true });
  }
  const t = c.tally;
  for (const problem of t.problems)
    process.stdout.write(`commit checker self-test: not as expected: ${problem}\n`);
  process.stdout.write(
    `commit checker self-test: planted ${String(t.planted)} caught ${String(t.caught)}, clean ${String(t.clean)} passed ${String(t.passed)}\n`,
  );
  const ok =
    t.planted > 0 &&
    t.clean > 0 &&
    t.caught === t.planted &&
    t.passed === t.clean &&
    t.problems.length === 0;
  return ok ? 0 : 1;
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  process.exitCode = main();
}
