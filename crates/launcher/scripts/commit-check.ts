// Checks commit messages, and a pull request's title, body and branch name,
// against the repository's commit rules, by their shape only: subject and
// body length, AI attribution strings, planning ids, local paths, private
// email addresses and never-commit files for every author, and on the
// maintainer's own commits the one co-author trailer and noreply addresses.
// Each commit is classified by its own metadata: commits Dependabot wrote and
// merge commits GitHub wrote are counted and exempt, the maintainer is the
// repository owner's noreply address, and every other author gets the rules
// every author owes.
//
//   node crates/launcher/scripts/commit-check.ts commits
//   node crates/launcher/scripts/commit-check.ts pr-text
//
// commits: in CI the range comes from the push or pull_request event in
// $GITHUB_EVENT_PATH; locally it is origin/main..HEAD plus the branch name.
// pr-text: the title, body and branch name come from the pull_request event
// file only; a Dependabot pull request's body is printed but not counted.
// Path shapes come from scripts/binary-audit.sh shapes; identity entries
// (CI_CHECK_IDENTITY, local only) are matched by its identity mode and never
// printed, and no hit line repeats the text it matched. This is detection:
// the local gate catches a hit before a push only when run; provider push
// protection is the only enforced preventive layer.
// Exit status: 0 pass, 1 a hit or a failed condition, 2 usage.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

interface PathShape {
  readonly label: string;
  readonly ere: string;
  readonly ignoreCase: boolean;
}

interface PathHit {
  // The index of the matching line in the lines passed in.
  readonly index: number;
  readonly label: string;
}

interface TextRecord {
  readonly location: string;
  readonly text: string;
}

interface IdentityResult {
  readonly present: boolean;
  readonly entries: number;
  readonly hits: { location: string; index: number }[];
  // The mode's presence and count lines, to be printed as they are.
  readonly lines: readonly string[];
}

// A condition that stops the check: the event, range, repository or a tool
// could not be read.
class CheckFailure extends Error {
  public constructor(message: string) {
    super(message);
    this.name = 'CheckFailure';
  }
}

const RS = '\u001e';
const US = '\u001f';
const MAX_BUFFER = 256 * 1024 * 1024;
const SUBJECT_LIMIT = 72;
const BODY_LIMIT = 8;
const DEPENDABOT = '49699333+dependabot[bot]@users.noreply.github.com';
const GITHUB_WRITER = 'noreply@github.com';
const NOREPLY_SUFFIX = '@users.noreply.github.com';

const RULES = [
  'subject length',
  'body length',
  'AI attribution',
  'planning id',
  'local path',
  'private email',
  'never-commit file',
  'co-author trailer',
  'noreply identity',
  'identity entry',
];

interface Ran {
  readonly status: number;
  readonly stdout: string;
}

const run = (command: string, args: readonly string[], cwd: string, input = ''): Ran => {
  const result = spawnSync(command, args, {
    cwd,
    input,
    encoding: 'utf8',
    maxBuffer: MAX_BUFFER,
    env: { ...process.env, LC_ALL: 'C.UTF-8' },
  });
  if (result.error !== undefined)
    throw new CheckFailure(`${command} could not be run (${result.error.message})`);
  return { status: result.status ?? 128, stdout: result.stdout };
};

const git = (args: readonly string[], cwd: string): Ran =>
  run('git', ['-c', 'i18n.logOutputEncoding=UTF-8', ...args], cwd);

const plural = (n: number, one: string, many: string): string =>
  `${String(n)} ${n === 1 ? one : many}`;

const nonBlank = (lines: readonly string[]): string[] => lines.filter((line) => line.trim() !== '');

function repoRoot(): string {
  const ran = git(['rev-parse', '--show-toplevel'], process.cwd());
  if (ran.status !== 0)
    throw new CheckFailure(
      `the working directory is not in a git repository (git rev-parse exited ${String(ran.status)})`,
    );
  return ran.stdout.trim();
}

const auditScriptPath = (): string =>
  fileURLToPath(new URL('../../../scripts/binary-audit.sh', import.meta.url));

// The path shapes whose scope includes text, from the audit script's list.
function readPathShapes(): PathShape[] {
  const ran = run('bash', [auditScriptPath(), 'shapes'], process.cwd());
  if (ran.status !== 0)
    throw new CheckFailure(`scripts/binary-audit.sh shapes exited ${String(ran.status)}`);
  const shapes: PathShape[] = [];
  for (const line of ran.stdout.split('\n')) {
    if (line === '') continue;
    const fields = line.split('\t');
    if (fields.length !== 5)
      throw new CheckFailure(
        `scripts/binary-audit.sh shapes printed a row of ${String(fields.length)} fields, not 5`,
      );
    const [label = '', kind = '', letterCase = '', scope = '', ere = ''] = fields;
    if (kind === 'path' && scope === 'binary+text')
      shapes.push({ label, ere, ignoreCase: letterCase === 'i' });
  }
  return shapes;
}

// Applies each shape with grep -E, the engine its expression is written for.
function matchPathShapes(shapes: readonly PathShape[], lines: readonly string[]): PathHit[] {
  if (lines.some((line) => line.includes('\n')))
    throw new CheckFailure('a line passed to the path shapes holds a newline');
  if (lines.length === 0) return [];
  const input = `${lines.join('\n')}\n`;
  const hits: PathHit[] = [];
  for (const shape of shapes) {
    const flags = shape.ignoreCase ? ['-n', '-a', '-E', '-i'] : ['-n', '-a', '-E'];
    const ran = run('grep', [...flags, '-e', shape.ere], process.cwd(), input);
    if (ran.status > 1)
      throw new CheckFailure(`grep exited ${String(ran.status)} on the shape ${shape.label}`);
    for (const line of ran.stdout.split('\n')) {
      const found = /^([0-9]+):/u.exec(line);
      if (found !== null) hits.push({ index: Number(found[1]) - 1, label: shape.label });
    }
  }
  return hits;
}

// Matches CI_CHECK_IDENTITY's entries against the records through the audit
// script's identity mode, which reads the variable itself.
function matchIdentity(records: readonly TextRecord[]): IdentityResult {
  if (records.some((item) => /[\t\n]/u.test(item.location) || item.text.includes('\n')))
    throw new CheckFailure('an identity record holds a newline, or a tab in its location');
  const input = records.map((item) => `${item.location}\t${item.text}\n`).join('');
  const ran = run('bash', [auditScriptPath(), 'identity'], process.cwd(), input);
  const lines = ran.stdout.split('\n').filter((line) => line !== '');
  if (ran.status > 1)
    throw new CheckFailure(
      `scripts/binary-audit.sh identity exited ${String(ran.status)}: ${lines.at(-1) ?? 'no output'}`,
    );
  const presence = lines.find((line) =>
    /^identity: (?:present, [0-9]+ entries|absent)$/u.test(line),
  );
  const counts = lines
    .map((line) => /^identity: records ([0-9]+) hits ([0-9]+)$/u.exec(line))
    .find((found) => found !== null);
  if (presence === undefined || counts === undefined)
    throw new CheckFailure('scripts/binary-audit.sh identity printed no presence or count line');
  const read = Number(counts[1]);
  if (read !== records.length)
    throw new CheckFailure(
      `scripts/binary-audit.sh identity read ${String(read)} records of ${String(records.length)}`,
    );
  const hits = lines
    .filter((line) => !line.startsWith('identity: '))
    .map((line) => {
      const cut = line.lastIndexOf('\t');
      return { location: line.slice(0, cut), index: Number(line.slice(cut + 1)) };
    });
  if (hits.length !== Number(counts[2]))
    throw new CheckFailure(
      'scripts/binary-audit.sh identity printed a hit count its lines differ from',
    );
  const entries = /present, ([0-9]+) entries/u.exec(presence);
  return {
    present: entries !== null,
    entries: Number(entries?.[1] ?? 0),
    hits,
    lines: [presence, counts[0]],
  };
}

// ---------------------------------------------------------------------------
// The shapes, one definition each.

const PLANNING_SHAPES: readonly { readonly kind: string; readonly shape: RegExp }[] = [
  { kind: 'ruling number', shape: /(?<![A-Za-z0-9_])P[0-9]+A[0-9]+(?![A-Za-z0-9_])/u },
  { kind: 'criterion number', shape: /(?<![A-Za-z0-9_])AC-P[0-9]+(?![A-Za-z0-9_])/u },
  { kind: 'plan name', shape: /(?<![A-Za-z0-9_])p[0-9]+-[A-Za-z0-9_]+(?![A-Za-z0-9_])/u },
  { kind: 'short id', shape: /(?<![A-Za-z0-9_])[RUDV][0-9]{1,2}(?![A-Za-z0-9_])/u },
  { kind: 'section sign', shape: /\xa7 ?[0-9]/u },
  { kind: 'planning directory', shape: /docs\/(?:route|plans)\//u },
];

// The kinds of planning id the line holds, in the order above.
function planningIdKinds(line: string): string[] {
  return PLANNING_SHAPES.filter((item) => item.shape.test(line)).map((item) => item.kind);
}

const EMAIL = /(?<![A-Za-z0-9._%+-])[A-Za-z][A-Za-z0-9._%+-]*@(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,}/gu;

const allowedEmail = (address: string): boolean => {
  const lower = address.toLowerCase();
  return (
    lower.endsWith(NOREPLY_SUFFIX) ||
    lower === GITHUB_WRITER ||
    /@example\.(?:com|org|net)$/u.test(lower)
  );
};

// The number of email addresses in the line outside the allowed forms.
function privateEmailCount(line: string): number {
  return [...line.matchAll(EMAIL)].filter((found) => !allowedEmail(found[0])).length;
}

const NEVER_COMMIT: readonly { readonly kind: string; readonly shape: RegExp }[] = [
  { kind: 'a CLAUDE.md file', shape: /(?:^|\/)CLAUDE\.md$/u },
  { kind: 'a config.toml file', shape: /(?:^|\/)config\.toml$/u },
  { kind: 'an .env file', shape: /(?:^|\/)\.env(?:\..*)?$/u },
  { kind: 'a key or certificate file', shape: /\.(?:pem|key)$/u },
  { kind: 'a file under docs/', shape: /^docs\//u },
];

const neverCommitKind = (file: string): string | null =>
  NEVER_COMMIT.find((item) => item.shape.test(file))?.kind ?? null;

// The lower-cased key of a "Key: value" line, or null.
const lineKey = (line: string): string | null =>
  /^\s*([A-Za-z][A-Za-z0-9-]*)\s*:/u.exec(line)?.[1]?.toLowerCase() ?? null;

// The kinds of AI attribution string the line holds.
const attributionKinds = (line: string): string[] => {
  const key = lineKey(line);
  const value = line.slice(line.indexOf(':') + 1);
  const kinds: string[] = [];
  if (key === 'claude-session') kinds.push('session trailer');
  if (line.includes('Generated with [')) kinds.push('generated-with link');
  if (line.includes('claude.ai/code/') || line.includes('claude.com/claude-code'))
    kinds.push('tool link');
  if (line.includes('\u{1F916}')) kinds.push('robot sign');
  if (
    key === 'co-authored-by' &&
    (/\b(?:claude|codex)\b/iu.test(value) || /@(?:anthropic|openai)\.com/iu.test(value))
  )
    kinds.push('AI co-author');
  return kinds;
};

const unique = (items: readonly string[]): string[] => [...new Set(items)];

// ---------------------------------------------------------------------------
// Commits.

interface Commit {
  readonly sha: string;
  readonly parents: readonly string[];
  readonly author: string;
  readonly committer: string;
  // %B, the raw message.
  readonly message: string;
  // %(trailers), the trailer block as git parses it.
  readonly trailers: string;
  // Paths the commit adds, copies or renames to (a merge: against its first parent).
  readonly added: readonly string[];
}

type CommitClass = 'dependabot' | 'github-merge' | 'maintainer' | 'other';

interface Hit {
  // A short SHA, "<short SHA> author email", "branch", ...
  readonly location: string;
  readonly rule: string;
  readonly detail: string;
}

const short = (sha: string): string => sha.slice(0, 12);

const LOG_FORMAT = `--format=${RS}%H${US}%P${US}%ae${US}%ce${US}%B${US}%(trailers)${US}`;

// One record of the log: seven fields, the last one the NUL-separated
// name-status entries. Null when it cannot be read.
const parseRecord = (chunk: string): Commit | null => {
  const fields = chunk.split(US);
  if (fields.length !== 7) return null;
  const [sha = '', parents = '', author = '', committer = '', message = '', trailers = ''] = fields;
  const rest = fields[6] ?? '';
  if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/u.test(sha) || !rest.startsWith('\0')) return null;
  const entries = rest.slice(1).replace(/^\n/u, '').split('\0');
  if (entries.at(-1) === '') entries.pop();
  const added: string[] = [];
  for (let i = 0; i < entries.length;) {
    const status = entries[i] ?? '';
    if (!/^[A-Z][0-9]*$/u.test(status)) return null;
    const pair = status.startsWith('R') || status.startsWith('C');
    const file = entries[i + (pair ? 2 : 1)];
    if (file === undefined) return null;
    if (/^[ACR]/u.test(status)) added.push(file);
    i += pair ? 3 : 2;
  }
  return {
    sha,
    parents: parents.split(' ').filter((parent) => parent !== ''),
    author,
    committer,
    message,
    trailers,
    added,
  };
};

// Every commit of the range in one git log pass. No --diff-filter: git log
// would then list only the commits that match it.
function readCommits(root: string, from: string, to: string): Commit[] {
  const range = `${from}..${to}`;
  const counted = git(['rev-list', '--count', range], root);
  const expected = Number(counted.stdout.trim());
  if (counted.status !== 0 || !Number.isInteger(expected))
    throw new CheckFailure(`git rev-list exited ${String(counted.status)} for the range`);
  const ran = git(
    ['log', LOG_FORMAT, '--diff-merges=first-parent', '--name-status', '-M', '-C', '-z', range],
    root,
  );
  if (ran.status !== 0)
    throw new CheckFailure(`git log exited ${String(ran.status)} for the range`);
  const chunks = ran.stdout.split(RS);
  const commits = chunks.slice(1).map(parseRecord);
  const readable = commits.filter((commit) => commit !== null);
  if (chunks[0] !== '' || readable.length !== commits.length || readable.length !== expected) {
    throw new CheckFailure(
      `git log gave ${String(readable.length)} readable records for ${String(expected)} commits in the range (a message holding a record or field separator byte cannot be read)`,
    );
  }
  return readable;
}

const isMaintainer = (email: string, owner: string): boolean =>
  email.toLowerCase().replace(/^[0-9]+\+/u, '') === `${owner.toLowerCase()}${NOREPLY_SUFFIX}`;

const classify = (commit: Commit, owner: string): CommitClass => {
  if (commit.author.toLowerCase() === DEPENDABOT) return 'dependabot';
  if (commit.parents.length >= 2 && commit.committer.toLowerCase() === GITHUB_WRITER)
    return 'github-merge';
  return isMaintainer(commit.author, owner) ? 'maintainer' : 'other';
};

const messageLines = (commit: Commit): string[] => commit.message.replace(/\n$/u, '').split('\n');

// The message without its trailer block: the trailer block is the last
// paragraph, so its non-blank line count is dropped from the end.
const withoutTrailers = (commit: Commit): string[] => {
  const lines = messageLines(commit);
  let drop = nonBlank(commit.trailers.split('\n')).length;
  while (drop > 0 && lines.length > 0) {
    if ((lines.pop() ?? '').trim() !== '') drop -= 1;
  }
  return lines;
};

const isNoreply = (email: string): boolean => {
  const lower = email.toLowerCase();
  return lower.endsWith(NOREPLY_SUFFIX) || lower === GITHUB_WRITER;
};

// The rules checked on one commit's own text and metadata; paths and identity
// entries are matched across the whole range afterwards.
function commitHits(commit: Commit, kind: CommitClass): Hit[] {
  const where = short(commit.sha);
  const lines = messageLines(commit);
  const hits: Hit[] = [];
  const add = (rule: string, detail: string): void => {
    hits.push({ location: where, rule, detail });
  };
  // Code points, not UTF-16 units: the limit counts characters.
  const subject = Array.from(lines[0] ?? '').length;
  if (subject > SUBJECT_LIMIT)
    add('subject length', `${String(subject)} characters, limit ${String(SUBJECT_LIMIT)}`);
  const trailerLines = nonBlank(commit.trailers.split('\n'));
  const body = nonBlank(lines.slice(1)).length - trailerLines.length;
  if (body > BODY_LIMIT) add('body length', `${String(body)} lines, limit ${String(BODY_LIMIT)}`);
  const attribution = unique(lines.flatMap(attributionKinds));
  if (attribution.length > 0) add('AI attribution', attribution.join(', '));
  const planning = unique(lines.flatMap(planningIdKinds));
  if (planning.length > 0) add('planning id', planning.join(', '));
  const emails = withoutTrailers(commit).reduce((sum, line) => sum + privateEmailCount(line), 0);
  if (emails > 0) add('private email', plural(emails, 'address', 'addresses'));
  const never = unique(commit.added.map(neverCommitKind).filter((item) => item !== null));
  if (never.length > 0) add('never-commit file', never.join(', '));
  if (kind !== 'maintainer') return hits;
  const coAuthors = lines.filter((line) => lineKey(line) === 'co-authored-by').length;
  const [trailer = ''] = trailerLines;
  if (
    trailerLines.length !== 1 ||
    !/^Co-Authored-By: (?:Rooty|Leafy)$/u.test(trailer.trim()) ||
    coAuthors > 1
  ) {
    add(
      'co-author trailer',
      `${plural(trailerLines.length, 'trailer line', 'trailer lines')} and ${plural(coAuthors, 'co-author line', 'co-author lines')}; the only trailer is one Co-Authored-By line naming Rooty or Leafy`,
    );
  }
  const notNoreply = [
    ...(isNoreply(commit.author) ? [] : ['author email']),
    ...(isNoreply(commit.committer) ? [] : ['committer email']),
  ];
  if (notNoreply.length > 0)
    add(
      'noreply identity',
      `${notNoreply.join(' and ')} ${notNoreply.length > 1 ? 'are' : 'is'} not a noreply address`,
    );
  return hits;
}

interface Range {
  readonly from: string;
  readonly to: string;
  readonly source: string;
  readonly owner: string;
  // The branch to check; null in CI (the pull request's own check reads it)
  // and on a detached HEAD.
  readonly branch: string | null;
  readonly notes: readonly string[];
}

const readEvent = (): unknown => {
  const file = process.env['GITHUB_EVENT_PATH'] ?? '';
  if (file === '')
    throw new CheckFailure(
      'GITHUB_EVENT_PATH is not set, so the event file with the range was not read',
    );
  try {
    return JSON.parse(readFileSync(file, 'utf8')) as unknown;
  } catch {
    throw new CheckFailure('the event file named by GITHUB_EVENT_PATH could not be read as JSON');
  }
};

// The value at a dotted path of the event, such as ".before".
const eventValue = (event: unknown, dotted: string): unknown => {
  let value: unknown = event;
  for (const key of dotted.split('.').slice(1)) {
    value =
      typeof value === 'object' && value !== null
        ? (value as Record<string, unknown>)[key]
        : undefined;
  }
  return value;
};

// A non-empty string at a dotted path of the event.
const eventField = (event: unknown, dotted: string): string => {
  const value = eventValue(event, dotted);
  if (typeof value !== 'string' || value === '')
    throw new CheckFailure(`the event file has no ${dotted}`);
  return value;
};

// A string at a dotted path of the event that may be empty; where allowed, a
// null reads as empty.
const eventString = (event: unknown, dotted: string, nullIsEmpty: boolean): string => {
  const value = eventValue(event, dotted);
  if (typeof value === 'string') return value;
  if (value === null && nullIsEmpty) return '';
  throw new CheckFailure(`the event file has no ${dotted}`);
};

const requireCommit = (root: string, field: string, sha: string): void => {
  if (
    !/^[0-9a-f]{40,64}$/u.test(sha) ||
    git(['cat-file', '-e', `${sha}^{commit}`], root).status !== 0
  )
    throw new CheckFailure(
      `the range is unavailable: ${field} ${short(sha)} is not a commit in this clone`,
    );
};

function ciRange(root: string): Range {
  const name = process.env['GITHUB_EVENT_NAME'] ?? '';
  const event = readEvent();
  if (name !== 'pull_request' && name !== 'push')
    throw new CheckFailure(
      `commit messages are checked on pull_request and push events, not ${name === '' ? 'an unnamed event' : name}`,
    );
  const fromField = name === 'push' ? '.before' : '.pull_request.base.sha';
  const toField = name === 'push' ? '.after' : '.pull_request.head.sha';
  const from = eventField(event, fromField);
  const to = eventField(event, toField);
  const owner = eventField(event, '.repository.owner.login');
  if (/^0+$/u.test(from))
    throw new CheckFailure(
      `the range is unavailable: ${fromField} is all zeros (a new branch or a history rewrite)`,
    );
  requireCommit(root, fromField, from);
  requireCommit(root, toField, to);
  if (name === 'push' && git(['merge-base', '--is-ancestor', from, to], root).status !== 0)
    throw new CheckFailure(
      `the range is unavailable: ${fromField} ${short(from)} is not an ancestor of ${toField} ${short(to)}`,
    );
  return { from, to, source: `${name} event`, owner, branch: null, notes: [] };
}

const OWNER_URLS: readonly RegExp[] = [
  /^https:\/\/github\.com\/([^/]+)\/[^/]+$/u,
  /^git@github\.com:([^/]+)\/[^/]+$/u,
  /^ssh:\/\/git@github\.com\/([^/]+)\/[^/]+$/u,
];

function localRange(root: string): Range {
  const main = git(['rev-parse', '-q', '--verify', 'origin/main^{commit}'], root);
  if (main.status !== 0) throw new CheckFailure('origin/main does not resolve to a commit');
  const head = git(['rev-parse', '-q', '--verify', 'HEAD^{commit}'], root);
  if (head.status !== 0) throw new CheckFailure('HEAD does not resolve to a commit');
  const url = git(['remote', 'get-url', 'origin'], root).stdout.trim();
  const owner = OWNER_URLS.map((shape) => shape.exec(url)?.[1]).find(
    (found) => found !== undefined,
  );
  if (owner === undefined)
    throw new CheckFailure(
      "the repository owner could not be read from origin's URL (a github.com https or ssh URL)",
    );
  const symbolic = git(['symbolic-ref', '--short', '-q', 'HEAD'], root);
  const branch = symbolic.status === 0 ? symbolic.stdout.trim() : null;
  return {
    from: main.stdout.trim(),
    to: head.stdout.trim(),
    source: 'origin/main..HEAD',
    owner,
    branch,
    notes: branch === null ? ['branch: not checked, because HEAD is detached'] : [],
  };
}

// Local paths and identity entries, matched over every checked commit's
// message lines (and emails, for identity) and the branch name at once.
function rangeHits(
  checked: readonly Commit[],
  branch: string | null,
): { readonly hits: Hit[]; readonly identity: IdentityResult } {
  const texts: TextRecord[] = [];
  const records: TextRecord[] = [];
  for (const commit of checked) {
    const where = short(commit.sha);
    for (const line of messageLines(commit)) {
      texts.push({ location: where, text: line });
      records.push({ location: where, text: line });
    }
    records.push({ location: `${where} author email`, text: commit.author });
    records.push({ location: `${where} committer email`, text: commit.committer });
  }
  if (branch !== null) {
    texts.push({ location: 'branch', text: branch });
    records.push({ location: 'branch', text: branch });
  }
  const shapes = readPathShapes();
  if (shapes.length === 0)
    throw new CheckFailure('scripts/binary-audit.sh shapes printed 0 path shapes for text');
  const hits: Hit[] = [];
  const paths = matchPathShapes(
    shapes,
    texts.map((item) => item.text),
  );
  const seen = new Set<string>();
  for (const found of paths) {
    const location = texts[found.index]?.location ?? '';
    const key = `${location}\t${found.label}`;
    if (!seen.has(key)) hits.push({ location, rule: 'local path', detail: found.label });
    seen.add(key);
  }
  const identity = matchIdentity(records);
  for (const found of identity.hits) {
    const key = `${found.location}\t${String(found.index)}`;
    if (!seen.has(key))
      hits.push({
        location: found.location,
        rule: 'identity entry',
        detail: `entry ${String(found.index)}`,
      });
    seen.add(key);
  }
  return { hits, identity };
}

const branchHits = (branch: string | null): Hit[] =>
  branch === null
    ? []
    : unique(planningIdKinds(branch)).map((kind) => ({
        location: 'branch',
        rule: 'planning id',
        detail: kind,
      }));

const hitLine = (hit: Hit): string =>
  hit.detail === ''
    ? `${hit.location}: ${hit.rule}`
    : `${hit.location}: ${hit.rule}: ${hit.detail}`;

const totalsLine = (rules: readonly string[], hits: readonly Hit[]): string =>
  `hits: ${rules.map((rule) => `${rule} ${String(hits.filter((hit) => hit.rule === rule).length)}`).join(', ')}`;

function checkCommits(write: (line: string) => void): number {
  const root = repoRoot();
  const range = process.env['GITHUB_ACTIONS'] === 'true' ? ciRange(root) : localRange(root);
  write(`commit range: ${short(range.from)}..${short(range.to)} (${range.source})`);
  const commits = readCommits(root, range.from, range.to);
  const classes = commits.map((commit) => classify(commit, range.owner));
  const count = (kind: CommitClass): string =>
    String(classes.filter((item) => item === kind).length);
  write(
    `commits in range: ${String(commits.length)} (dependabot ${count('dependabot')}, github merges ${count('github-merge')}, maintainer ${count('maintainer')}, other ${count('other')})`,
  );
  for (const note of range.notes) write(note);
  if (commits.length === 0) throw new CheckFailure('the range holds 0 commits');
  const checked = commits.filter(
    (_commit, i) => classes[i] === 'maintainer' || classes[i] === 'other',
  );
  const own = checked.flatMap((commit) => commitHits(commit, classify(commit, range.owner)));
  const matched = rangeHits(checked, range.branch);
  const order = new Map(commits.map((commit, i) => [short(commit.sha), i]));
  const position = (hit: Hit): number =>
    order.get(hit.location.split(' ')[0] ?? '') ?? commits.length;
  const hits = [...own, ...matched.hits, ...branchHits(range.branch)].sort(
    (a, b) => position(a) - position(b) || RULES.indexOf(a.rule) - RULES.indexOf(b.rule),
  );
  for (const hit of hits) write(hitLine(hit));
  write(totalsLine(RULES, hits));
  for (const line of matched.identity.lines) write(line);
  return hits.length === 0 ? 0 : 1;
}

// ---------------------------------------------------------------------------
// The pull request's title, body and branch name.

const PR_RULES = [
  'AI attribution',
  'planning id',
  'test plan heading',
  'local path',
  'private email',
];
const TEST_PLAN = /^\s*(?:#{1,6}\s*|\*\*)?test plan\b/iu;
const DEPENDABOT_LOGIN = 'dependabot[bot]';

// The rules of one field's line; the branch name has no attribution, heading
// or email rule, the title no heading rule.
const prLineHits = (item: TextRecord): Hit[] => {
  const hits: Hit[] = [];
  const add = (rule: string, detail: string): void => {
    hits.push({ location: item.location, rule, detail });
  };
  const branch = item.location === 'branch';
  const attribution = branch ? [] : unique(attributionKinds(item.text));
  if (attribution.length > 0) add('AI attribution', attribution.join(', '));
  const planning = unique(planningIdKinds(item.text));
  if (planning.length > 0) add('planning id', planning.join(', '));
  if (item.location.startsWith('pr-body:') && TEST_PLAN.test(item.text))
    add('test plan heading', '');
  const emails = branch ? 0 : privateEmailCount(item.text);
  if (emails > 0) add('private email', plural(emails, 'address', 'addresses'));
  return hits;
};

function checkPrText(write: (line: string) => void): number {
  const name = process.env['GITHUB_EVENT_NAME'] ?? '';
  if (name !== 'pull_request')
    throw new CheckFailure(
      `the PR text is read from a pull_request event, not ${name === '' ? 'an unnamed event' : name}`,
    );
  const event = readEvent();
  const title = eventString(event, '.pull_request.title', false).replaceAll(/\r?\n/gu, ' ');
  const body = eventString(event, '.pull_request.body', true).replaceAll('\r\n', '\n');
  const branch = eventField(event, '.pull_request.head.ref');
  const exempt = eventField(event, '.pull_request.user.login') === DEPENDABOT_LOGIN;
  const bodyLines = body === '' ? [] : body.replace(/\n$/u, '').split('\n');
  const items: TextRecord[] = [
    { location: 'pr-title', text: title },
    ...bodyLines.map((text, i) => ({ location: `pr-body:${String(i + 1)}`, text })),
    { location: 'branch', text: branch },
  ];
  const shapes = readPathShapes();
  if (shapes.length === 0)
    throw new CheckFailure('scripts/binary-audit.sh shapes printed 0 path shapes for text');
  const paths = matchPathShapes(
    shapes,
    items.map((item) => item.text),
  ).map((found) => ({
    location: items[found.index]?.location ?? '',
    rule: 'local path',
    detail: found.label,
  }));
  const seen = new Set<string>();
  const order = new Map(items.map((item, i) => [item.location, i]));
  const hits = [...items.flatMap(prLineHits), ...paths]
    .filter((hit) => {
      const key = `${hit.location}\t${hit.rule}\t${hit.detail}`;
      const fresh = !seen.has(key);
      seen.add(key);
      return fresh;
    })
    .sort(
      (a, b) =>
        (order.get(a.location) ?? 0) - (order.get(b.location) ?? 0) ||
        PR_RULES.indexOf(a.rule) - PR_RULES.indexOf(b.rule),
    );
  const counted = (hit: Hit): boolean => !(exempt && hit.location.startsWith('pr-body:'));
  const notCounted = hits.filter((hit) => !counted(hit)).length;
  const exemption = exempt ? `yes, ${plural(notCounted, 'hit', 'hits')} not counted` : 'no';
  write(
    `pr text: fields read 3 (title ${plural(Array.from(title).length, 'character', 'characters')}, body ${plural(bodyLines.length, 'line', 'lines')}, branch); dependabot body exemption: ${exemption}`,
  );
  for (const hit of hits) write(counted(hit) ? hitLine(hit) : `${hitLine(hit)} (not counted)`);
  write(totalsLine(PR_RULES, hits.filter(counted)));
  if (title === '') {
    write('pr text: FAIL the title read from the event file is empty');
    return 1;
  }
  return hits.some(counted) ? 1 : 0;
}

const USAGE = 'usage: commit-check.ts commits | pr-text';

const main = (args: readonly string[]): number => {
  const write = (line: string): void => {
    process.stdout.write(`${line}\n`);
  };
  const [mode = ''] = args;
  if (args.length !== 1 || (mode !== 'commits' && mode !== 'pr-text')) {
    process.stderr.write(`${USAGE}\n`);
    return 2;
  }
  try {
    return mode === 'commits' ? checkCommits(write) : checkPrText(write);
  } catch (error: unknown) {
    if (!(error instanceof CheckFailure)) throw error;
    write(`${mode === 'commits' ? 'commit messages' : 'pr text'}: FAIL ${error.message}`);
    return 1;
  }
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  process.exitCode = main(process.argv.slice(2));
}
