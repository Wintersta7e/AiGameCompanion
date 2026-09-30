// Checks that the workflows call scripts/ci-check.sh the way the step table
// expects: one runner call per job, every table tag called by one job, an
// aggregate job that fails on any result but success, the triggers, colour
// settings, the required job names, --locked on every cargo call that reads
// the dependency graph, read-only token permissions, and text that must not
// come back (a fetching front door, a retired tool, a claim of equal checks).
//
//   node crates/launcher/scripts/wiring-check.ts
//
// It reads every file in .github/workflows/ with its own parser for the
// constrained YAML shape these files use, and the table through
// `bash scripts/ci-check.sh --list`. A shape it does not recognise fails.
import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

export type YamlValue = string | null | readonly YamlValue[] | YamlMapping;
export type YamlMapping = ReadonlyMap<string, YamlValue>;

export class UnrecognisedShape extends Error {
  public constructor(file: string, line: number, what: string) {
    super(`${file}:${line}: ${what}`);
    this.name = 'UnrecognisedShape';
  }
}

export interface Job {
  readonly id: string;
  readonly map: YamlMapping;
  readonly steps: readonly YamlMapping[];
}

export interface Workflow {
  readonly file: string;
  readonly root: YamlMapping;
  readonly jobs: readonly Job[];
}

export interface ListRow {
  readonly name: string;
  readonly tags: readonly string[];
  readonly category: string;
  readonly cmd: string;
}

export interface WiringInput {
  readonly workflows: readonly Workflow[];
  readonly unrecognised: readonly string[];
  readonly listText: string;
  // Exit statuses of the aggregate job's run block for AGGREGATE_PAYLOADS, in
  // order; null when ci.yml has no aggregate run block to execute.
  readonly aggregateStatuses: readonly number[] | null;
  readonly requiredNames?: readonly string[];
  // scripts/build.sh, whose cargo calls the --locked check reads too.
  readonly buildScript: string;
  // Every tracked file's path and text, for the banned-text checks.
  readonly trackedFiles: readonly TrackedFile[];
}

export interface TrackedFile {
  readonly path: string;
  readonly text: string;
}

export interface Failure {
  readonly condition: string;
  readonly message: string;
}

export interface WiringResult {
  readonly failures: readonly Failure[];
  readonly counts: string;
}

export const NON_BLOCKING_JOBS: readonly string[] = [
  'canary-linux',
  'canary-windows',
  'freshness',
  'coverage',
];
export const AGGREGATE_JOB = 'aggregate';
export const REQUIRED_JOB_NAMES: readonly string[] = [
  'Rust (fmt, clippy, test)',
  'Rust (non-Windows build)',
  'Frontend (lint, types, build)',
  'Release build + binary audit',
  'Supply chain (cargo-deny)',
  'Workflows (actionlint)',
  'Secrets (gitleaks)',
  'Result of the needed CI jobs',
];
export const AGGREGATE_PAYLOADS: readonly string[] = [
  '{"rust":{"result":"success","outputs":{}},"secrets":{"result":"success","outputs":{}}}',
  '{"rust":{"result":"failure","outputs":{}},"secrets":{"result":"success","outputs":{}}}',
  '{"rust":{"result":"cancelled","outputs":{}},"secrets":{"result":"success","outputs":{}}}',
  '{"rust":{"result":"skipped","outputs":{}},"secrets":{"result":"success","outputs":{}}}',
  '{}',
];

const CALL = /^\.\/scripts\/ci-check\.sh --job ([a-z][a-z0-9-]*)$/u;
const MATCHER_STEP = 'echo "::add-matcher::.github/problem-matchers.json"';
const CONCURRENCY_GROUP = `\${{ github.event_name == 'pull_request' && format('{0}-pr-{1}', github.workflow, github.ref) || format('{0}-run-{1}', github.workflow, github.run_id) }}`;
const CONCURRENCY_CANCEL = `\${{ github.event_name == 'pull_request' }}`;
const WORKFLOW_KEYS = new Set(['name', 'on', 'permissions', 'concurrency', 'env', 'jobs']);
const JOB_KEYS = new Set([
  'name',
  'if',
  'needs',
  'runs-on',
  'timeout-minutes',
  'permissions',
  'defaults',
  'env',
  'steps',
  'continue-on-error',
]);
const STEP_KEYS = new Set([
  'name',
  'id',
  'if',
  'uses',
  'with',
  'env',
  'run',
  'working-directory',
  'continue-on-error',
]);

// ---------------------------------------------------------------------------
// The parser: 2-space block mappings and sequences, plain and quoted scalars,
// flow sequences, `{}`, `|` block scalars and comments. Nothing else.

interface Line {
  readonly no: number;
  readonly indent: number;
  readonly text: string;
  readonly raw: string;
}

interface Cursor {
  i: number;
}

interface Source {
  readonly file: string;
  readonly lines: readonly Line[];
  readonly cur: Cursor;
}

const stripComment = (text: string): string => {
  let quote = '';
  for (let i = 0; i < text.length; i += 1) {
    const c = text.charAt(i);
    if (quote !== '') {
      if (c === quote) quote = '';
    } else if (c === "'" || c === '"') {
      quote = c;
    } else if (c === '#' && (i === 0 || text.charAt(i - 1) === ' ')) {
      return text.slice(0, i).trimEnd();
    }
  }
  return text.trimEnd();
};

const toLines = (text: string, file: string): Line[] =>
  text.split('\n').map((raw, index) => {
    if (raw.includes('\t')) throw new UnrecognisedShape(file, index + 1, 'a tab');
    const indent = raw.length - raw.trimStart().length;
    return { no: index + 1, indent, text: stripComment(raw.slice(indent)), raw };
  });

const fail = (src: Source, line: Line | undefined, what: string): UnrecognisedShape =>
  new UnrecognisedShape(src.file, line?.no ?? src.lines.length, what);

const skipBlank = (src: Source): void => {
  while ((src.lines[src.cur.i]?.text ?? 'end') === '') src.cur.i += 1;
};

const isItem = (text: string): boolean => text === '-' || text.startsWith('- ');

const unquote = (src: Source, line: Line | undefined, text: string): string => {
  const quote = text.charAt(0);
  if (text.length < 2 || !text.endsWith(quote))
    throw fail(src, line, 'an unterminated quoted scalar');
  const body = text.slice(1, -1);
  if (quote === "'") return body.replaceAll("''", "'");
  return body.replaceAll('\\"', '"').replaceAll('\\\\', '\\');
};

const scalar = (src: Source, line: Line | undefined, text: string): YamlValue => {
  if (text === '{}') return new Map<string, YamlValue>();
  if (text === '---' || /^[&*!{>|%@`]/u.test(text))
    throw fail(src, line, `an unsupported value "${text}"`);
  if (text.startsWith('[')) {
    if (!text.endsWith(']') || text.slice(1, -1).includes('['))
      throw fail(src, line, 'a nested flow sequence');
    const inner = text.slice(1, -1).trim();
    return inner === '' ? [] : inner.split(',').map((item) => scalar(src, line, item.trim()));
  }
  if (text.startsWith("'") || text.startsWith('"')) return unquote(src, line, text);
  return text;
};

const literalBlock = (src: Source, parentIndent: number): string => {
  const body: string[] = [];
  let blockIndent = -1;
  for (;;) {
    const line = src.lines[src.cur.i];
    if (line === undefined) break;
    const blank = line.raw.trim() === '';
    if (!blank && line.indent <= parentIndent) break;
    if (!blank && blockIndent < 0) blockIndent = line.indent;
    body.push(blank ? '' : line.raw.slice(Math.max(blockIndent, 0)));
    src.cur.i += 1;
  }
  while (body.at(-1) === '') body.pop();
  return `${body.join('\n')}\n`;
};

const entryValue = (src: Source, indent: number, rest: string, line: Line): YamlValue => {
  if (rest === '|') return literalBlock(src, indent);
  if (rest !== '') return scalar(src, line, rest);
  skipBlank(src);
  const next = src.lines[src.cur.i];
  return next !== undefined && next.indent > indent ? block(src, next.indent) : null;
};

const entry = (src: Source, indent: number, text: string, into: Map<string, YamlValue>): void => {
  const line = src.lines[src.cur.i];
  const found = /^([A-Za-z0-9_-]+):(?: (.*))?$/u.exec(text);
  if (found === null || line === undefined)
    throw fail(src, line, 'a line that is not a key: value entry');
  const key = found[1] ?? '';
  if (into.has(key)) throw fail(src, line, `the key ${key} twice`);
  src.cur.i += 1;
  into.set(key, entryValue(src, indent, (found[2] ?? '').trim(), line));
};

const mapping = (src: Source, indent: number, into: Map<string, YamlValue>): YamlMapping => {
  for (;;) {
    skipBlank(src);
    const line = src.lines[src.cur.i];
    if (line === undefined || line.indent < indent) return into;
    if (line.indent > indent || isItem(line.text))
      throw fail(src, line, 'an unexpected indentation');
    entry(src, indent, line.text, into);
  }
};

const sequence = (src: Source, indent: number): YamlValue[] => {
  const items: YamlValue[] = [];
  for (;;) {
    skipBlank(src);
    const line = src.lines[src.cur.i];
    if (line === undefined || line.indent < indent) return items;
    if (line.indent > indent || !isItem(line.text))
      throw fail(src, line, 'an unexpected indentation');
    const body = line.text.slice(1).trimStart();
    if (/^[A-Za-z0-9_-]+:(?: |$)/u.test(body)) {
      const item = new Map<string, YamlValue>();
      entry(src, indent + 2, body, item);
      items.push(mapping(src, indent + 2, item));
    } else if (body === '') {
      throw fail(src, line, 'an empty sequence item');
    } else {
      src.cur.i += 1;
      items.push(scalar(src, line, body));
    }
  }
};

const block = (src: Source, indent: number): YamlValue => {
  skipBlank(src);
  const line = src.lines[src.cur.i];
  if (line === undefined) return null;
  return isItem(line.text)
    ? sequence(src, indent)
    : mapping(src, indent, new Map<string, YamlValue>());
};

const isMapping = (value: YamlValue | undefined): value is YamlMapping => value instanceof Map;

const asMapping = (value: YamlValue | undefined): YamlMapping =>
  isMapping(value) ? value : new Map<string, YamlValue>();

const isList = (value: YamlValue | undefined): value is readonly YamlValue[] =>
  Array.isArray(value);

const asList = (value: YamlValue | undefined): readonly YamlValue[] => (isList(value) ? value : []);

const asText = (value: YamlValue | undefined): string | null =>
  typeof value === 'string' ? value : null;

const knownKeys = (
  file: string,
  map: YamlMapping,
  known: ReadonlySet<string>,
  where: string,
): void => {
  for (const key of map.keys()) {
    if (!known.has(key)) throw new UnrecognisedShape(file, 0, `the key "${key}" in ${where}`);
  }
};

export function parseWorkflow(text: string, file: string): Workflow {
  const src: Source = { file, lines: toLines(text, file), cur: { i: 0 } };
  const root = asMapping(block(src, 0));
  skipBlank(src);
  if (src.cur.i < src.lines.length)
    throw fail(src, src.lines[src.cur.i], 'text after the document');
  knownKeys(file, root, WORKFLOW_KEYS, 'the workflow');
  const jobs = [...asMapping(root.get('jobs')).entries()].map(([id, value]) => {
    if (!isMapping(value)) throw new UnrecognisedShape(file, 0, `job ${id} is not a mapping`);
    knownKeys(file, value, JOB_KEYS, `job ${id}`);
    const steps = asList(value.get('steps')).map((step) => {
      if (!isMapping(step))
        throw new UnrecognisedShape(file, 0, `a step of job ${id} is not a mapping`);
      knownKeys(file, step, STEP_KEYS, `a step of job ${id}`);
      return step;
    });
    return { id, map: value, steps };
  });
  return { file, root, jobs };
}

// ---------------------------------------------------------------------------
// The conditions.

export const parseList = (text: string): ListRow[] =>
  text
    .split('\n')
    .filter((line) => line.startsWith('row\t'))
    .map((line) => {
      const fields = line.split('\t');
      return {
        name: fields[1] ?? '',
        tags: (fields[2] ?? '').split(',').filter((tag) => tag !== ''),
        category: fields[3] ?? '',
        cmd: fields[12] ?? '',
      };
    });

const isCheckout = (step: YamlMapping): boolean =>
  (asText(step.get('uses')) ?? '').startsWith('actions/checkout@');

const callTag = (step: YamlMapping): string | null =>
  CALL.exec(asText(step.get('run')) ?? '')?.[1] ?? null;

const baseName = (file: string): string => path.basename(file);

const ciWorkflow = (input: WiringInput): Workflow | undefined =>
  input.workflows.find((workflow) => baseName(workflow.file) === 'ci.yml');

const bare = (expression: string): string =>
  expression
    .replace(/^\$\{\{\s*(.*?)\s*\}\}$/u, '$1')
    .replaceAll(/\s+/gu, ' ')
    .trim();

const allJobs = (input: WiringInput): { readonly workflow: Workflow; readonly job: Job }[] =>
  input.workflows.flatMap((workflow) => workflow.jobs.map((job) => ({ workflow, job })));

// Each job but the aggregate makes exactly one plain runner call.
const checkCalls = (input: WiringInput): Failure[] =>
  allJobs(input)
    .filter(({ job }) => job.id !== AGGREGATE_JOB)
    .flatMap(({ workflow, job }) => {
      const callLike = job.steps.filter((step) =>
        (asText(step.get('run')) ?? '').includes('scripts/ci-check.sh'),
      );
      const exact = callLike.filter((step) => callTag(step) !== null);
      const where = `${baseName(workflow.file)} job ${job.id}`;
      if (callLike.length !== 1 || exact.length !== 1) {
        return [
          {
            condition: 'runner call',
            message: `${where} has ${exact.length} plain runner calls among ${callLike.length} runner steps, not 1`,
          },
        ];
      }
      const guarded = exact.filter((step) => step.has('if') || step.has('continue-on-error'));
      return guarded.length === 0
        ? []
        : [
            {
              condition: 'runner call',
              message: `${where}: the runner call carries if or continue-on-error`,
            },
          ];
    });

const callersOf = (input: WiringInput): Map<string, string[]> => {
  const callers = new Map<string, string[]>();
  for (const { job } of allJobs(input)) {
    for (const step of job.steps) {
      const tag = callTag(step);
      if (tag !== null) callers.set(tag, [...(callers.get(tag) ?? []), job.id]);
    }
  }
  return callers;
};

// Every called tag is in the table; a blocking tag has exactly one
// blocking caller (a second only from the non-blocking list); a non-blocking tag
// (one with a non-blocking row) has exactly one caller, from that list.
const checkTags = (
  rows: readonly ListRow[],
  callers: ReadonlyMap<string, readonly string[]>,
): Failure[] => {
  const tags = new Set(rows.flatMap((row) => row.tags));
  const failures: Failure[] = [];
  for (const tag of callers.keys()) {
    if (!tags.has(tag))
      failures.push({
        condition: 'tag callers',
        message: `the tag ${tag} is called but no table row carries it`,
      });
  }
  for (const tag of tags) {
    const who = callers.get(tag) ?? [];
    const nonBlocking = rows.some(
      (row) => row.category === 'non-blocking' && row.tags.includes(tag),
    );
    const blockingCallers = who.filter((job) => !NON_BLOCKING_JOBS.includes(job));
    const wrong = nonBlocking
      ? who.length !== 1 || blockingCallers.length !== 0
      : blockingCallers.length !== 1;
    if (wrong) {
      const kind = nonBlocking ? 'non-blocking' : 'blocking';
      failures.push({
        condition: 'tag callers',
        message: `the ${kind} tag ${tag} has ${who.length} callers (${who.join(', ') || 'none'})`,
      });
    }
  }
  return failures;
};

// The aggregate job needs every other blocking job of ci.yml.
const checkAggregate = (ci: Workflow | undefined): Failure[] => {
  const job = ci?.jobs.find((candidate) => candidate.id === AGGREGATE_JOB);
  if (ci === undefined || job === undefined)
    return [{ condition: 'aggregate', message: 'ci.yml has no aggregate job' }];
  const needs = asList(job.map.get('needs')).map((value) => asText(value) ?? '');
  const wanted = ci.jobs
    .map((candidate) => candidate.id)
    .filter((id) => id !== AGGREGATE_JOB && !NON_BLOCKING_JOBS.includes(id));
  const failures: Failure[] = [];
  const missing = wanted.filter((id) => !needs.includes(id));
  const extra = needs.filter((id) => !wanted.includes(id));
  if (missing.length > 0 || extra.length > 0) {
    failures.push({
      condition: 'aggregate',
      message: `ci.yml aggregate needs ${needs.length} jobs; missing ${missing.join(', ') || 'none'}, extra ${extra.join(', ') || 'none'}`,
    });
  }
  if (bare(asText(job.map.get('if')) ?? '') !== 'always()')
    failures.push({ condition: 'aggregate', message: 'ci.yml aggregate lacks if: always()' });
  const permissions = job.map.get('permissions');
  if (!isMapping(permissions) || permissions.size !== 0)
    failures.push({ condition: 'aggregate', message: 'ci.yml aggregate lacks permissions: {}' });
  if (job.steps.some((step) => step.has('uses')))
    failures.push({
      condition: 'aggregate',
      message: 'ci.yml aggregate has a checkout or another uses: step',
    });
  return failures;
};

const crons = (ci: Workflow | undefined): string[] =>
  asList(asMapping(ci?.root.get('on')).get('schedule')).map(
    (item) => asText(asMapping(item).get('cron')) ?? '',
  );

// The schedule line whose day-of-week field is a single day.
const weeklyCron = (ci: Workflow | undefined): string | null =>
  crons(ci).find((cron) => /^[0-7]$/u.test(cron.split(' ')[4] ?? '')) ?? null;

// Only the canaries and the freshness job carry a job-level if:,
// and theirs names the weekly cron line.
const checkJobIfs = (input: WiringInput): Failure[] => {
  const allowed = ['canary-linux', 'canary-windows', 'freshness'];
  const expected = `github.event_name == 'workflow_dispatch' || github.event.schedule == '${weeklyCron(ciWorkflow(input)) ?? ''}'`;
  return allJobs(input).flatMap(({ workflow, job }) => {
    const condition = asText(job.map.get('if'));
    if (condition === null || job.id === AGGREGATE_JOB) return [];
    const where = `${baseName(workflow.file)} job ${job.id}`;
    if (!allowed.includes(job.id))
      return [{ condition: 'job if', message: `${where} has a job-level if:` }];
    return bare(condition) === expected
      ? []
      : [{ condition: 'job if', message: `${where}: its if: is not the weekly-run form` }];
  });
};

// Timeout, bash, the matcher step and a name on every step.
const checkJobShape = (input: WiringInput): Failure[] =>
  allJobs(input).flatMap(({ workflow, job }) => {
    const where = `${baseName(workflow.file)} job ${job.id}`;
    const failures: Failure[] = [];
    if (!job.map.has('timeout-minutes'))
      failures.push({ condition: 'job shape', message: `${where} has no timeout-minutes` });
    const shell = asText(asMapping(asMapping(job.map.get('defaults')).get('run')).get('shell'));
    if (shell !== 'bash')
      failures.push({
        condition: 'job shape',
        message: `${where} has no defaults.run.shell: bash`,
      });
    const matcher = job.steps.some((step) => asText(step.get('run')) === MATCHER_STEP);
    if (job.id !== AGGREGATE_JOB && !matcher)
      failures.push({
        condition: 'job shape',
        message: `${where} has no matcher registration step`,
      });
    const unnamed = job.steps.filter((step) => !step.has('name')).length;
    if (unnamed > 0)
      failures.push({
        condition: 'job shape',
        message: `${where} has ${unnamed} steps without a name`,
      });
    return failures;
  });

const mentions = (value: YamlValue | undefined, needle: string): boolean => {
  if (typeof value === 'string') return value.includes(needle);
  if (Array.isArray(value)) return value.some((item: YamlValue) => mentions(item, needle));
  if (isMapping(value))
    return [...value.entries()].some(
      ([key, item]) => key.includes(needle) || mentions(item, needle),
    );
  return false;
};

// No continue-on-error anywhere; no toolchain override in a needed job.
const checkNoEscapes = (input: WiringInput): Failure[] => {
  const ci = ciWorkflow(input);
  const needed = asList(ci?.jobs.find((job) => job.id === AGGREGATE_JOB)?.map.get('needs')).map(
    (value) => asText(value),
  );
  return allJobs(input).flatMap(({ workflow, job }) => {
    const where = `${baseName(workflow.file)} job ${job.id}`;
    const failures: Failure[] = [];
    const escapes = [job.map, ...job.steps].filter((map) => map.has('continue-on-error')).length;
    if (escapes > 0)
      failures.push({
        condition: 'escape',
        message: `${where} has continue-on-error ${escapes} times`,
      });
    if (workflow === ci && needed.includes(job.id) && mentions(job.map, 'RUSTUP_TOOLCHAIN')) {
      failures.push({
        condition: 'escape',
        message: `${where} is needed by the aggregate job and sets RUSTUP_TOOLCHAIN`,
      });
    }
    return failures;
  });
};

const sameSet = (a: readonly string[], b: readonly string[]): boolean =>
  a.length === b.length && a.every((item) => b.includes(item));

// In ci.yml: push to main, bare pull_request and workflow_dispatch,
// two schedule lines at one time of day (minute not 0), and the concurrency
// that never cancels or replaces a run outside pull requests.
const checkCiTriggers = (ci: Workflow | undefined): Failure[] => {
  if (ci === undefined) return [{ condition: 'triggers', message: 'there is no ci.yml' }];
  const on = asMapping(ci.root.get('on'));
  const failures: Failure[] = [];
  const branches = asList(asMapping(on.get('push')).get('branches')).map(
    (value) => asText(value) ?? '',
  );
  const shapeOk =
    sameSet([...on.keys()], ['push', 'pull_request', 'workflow_dispatch', 'schedule']) &&
    asMapping(on.get('push')).size === 1 &&
    sameSet(branches, ['main']) &&
    on.get('pull_request') === null &&
    on.get('workflow_dispatch') === null;
  if (!shapeOk)
    failures.push({
      condition: 'triggers',
      message: 'ci.yml on: is not push to main, pull_request, workflow_dispatch and schedule',
    });
  const times = crons(ci).map((cron) => cron.split(' ').slice(0, 2).join(' '));
  const minute = times[0]?.split(' ')[0] ?? '0';
  if (times.length !== 2 || times[0] !== times[1] || minute === '0' || !/^\d+$/u.test(minute)) {
    failures.push({
      condition: 'triggers',
      message: `ci.yml has ${times.length} schedule lines; two at one hour, minute not 0, are required`,
    });
  }
  const concurrency = asMapping(ci.root.get('concurrency'));
  if (
    asText(concurrency.get('group')) !== CONCURRENCY_GROUP ||
    asText(concurrency.get('cancel-in-progress')) !== CONCURRENCY_CANCEL
  ) {
    failures.push({
      condition: 'triggers',
      message: 'ci.yml concurrency is not the per-run form that cancels only pull request runs',
    });
  }
  return failures;
};

// The PR-text workflow when it exists.
const checkPrText = (input: WiringInput): Failure[] => {
  const pr = input.workflows.find((workflow) => baseName(workflow.file) === 'pr-text.yml');
  if (pr === undefined) return [];
  const on = asMapping(pr.root.get('on'));
  const types = asList(asMapping(on.get('pull_request')).get('types')).map(
    (value) => asText(value) ?? '',
  );
  const failures: Failure[] = [];
  if (on.size !== 1 || !sameSet(types, ['opened', 'edited', 'synchronize', 'reopened'])) {
    failures.push({
      condition: 'triggers',
      message: 'pr-text.yml on: is not pull_request with opened, edited, synchronize, reopened',
    });
  }
  const concurrency = asMapping(pr.root.get('concurrency'));
  if (
    !(asText(concurrency.get('group')) ?? '').includes('github.event.pull_request.number') ||
    asText(concurrency.get('cancel-in-progress')) !== 'true'
  ) {
    failures.push({
      condition: 'triggers',
      message: 'pr-text.yml concurrency is not per pull request with cancel-in-progress: true',
    });
  }
  return failures;
};

// Colour off in every workflow; no row forces colour.
const checkColour = (input: WiringInput, rows: readonly ListRow[]): Failure[] => {
  const failures: Failure[] = [];
  for (const workflow of input.workflows) {
    const env = asMapping(workflow.root.get('env'));
    if (asText(env.get('CARGO_TERM_COLOR')) !== 'never' || asText(env.get('NO_COLOR')) !== '1') {
      failures.push({
        condition: 'colour',
        message: `${baseName(workflow.file)} env lacks CARGO_TERM_COLOR: never and NO_COLOR: "1"`,
      });
    }
  }
  for (const row of rows) {
    if (/(?:^|\s)-color(?:\s|$)|--color[= ]always/u.test(row.cmd)) {
      failures.push({ condition: 'colour', message: `the row ${row.name} forces colour` });
    }
  }
  return failures;
};

// A non-blocking row's tag is called only by non-blocking jobs.
const checkNonBlockingRows = (
  rows: readonly ListRow[],
  callers: ReadonlyMap<string, readonly string[]>,
): Failure[] =>
  rows
    .filter((row) => row.category === 'non-blocking')
    .flatMap((row) => row.tags)
    .flatMap((tag) =>
      (callers.get(tag) ?? [])
        .filter((job) => !NON_BLOCKING_JOBS.includes(job))
        .map((job) => ({
          condition: 'non-blocking',
          message: `the non-blocking tag ${tag} is called by the needed job ${job}`,
        })),
    );

// The aggregate block passes only the all-success payload.
const checkPayloads = (statuses: readonly number[] | null): Failure[] => {
  if (statuses?.length !== AGGREGATE_PAYLOADS.length) {
    return [
      {
        condition: 'payloads',
        message: 'the aggregate run block was not executed against the five payloads',
      },
    ];
  }
  const wrong = statuses.filter((status, index) =>
    index === 0 ? status !== 0 : status === 0,
  ).length;
  return wrong === 0
    ? []
    : [
        {
          condition: 'payloads',
          message: `the aggregate run block judged ${wrong} of 5 payloads wrongly`,
        },
      ];
};

// The required contexts: each name once as a job name in ci.yml.
const checkNames = (ci: Workflow | undefined, names: readonly string[]): Failure[] =>
  names.flatMap((name) => {
    const count = (ci?.jobs ?? []).filter((job) => asText(job.map.get('name')) === name).length;
    return count === 1
      ? []
      : [
          {
            condition: 'names',
            message: `the required job name "${name}" occurs ${count} times in ci.yml`,
          },
        ];
  });

const countLine = (
  input: WiringInput,
  rows: readonly ListRow[],
  callers: ReadonlyMap<string, readonly string[]>,
  extra: readonly string[],
): string => {
  const jobs = allJobs(input);
  const aggregate = ciWorkflow(input)?.jobs.find((job) => job.id === AGGREGATE_JOB);
  const parts = [
    `workflow files ${input.workflows.length + input.unrecognised.length}`,
    `jobs ${jobs.length}`,
    `checkouts ${jobs.flatMap(({ job }) => job.steps).filter(isCheckout).length}`,
    `runner calls ${jobs.flatMap(({ job }) => job.steps).filter((step) => callTag(step) !== null).length}`,
    `tags in table ${new Set(rows.flatMap((row) => row.tags)).size}`,
    `tags called ${callers.size}`,
    `needs ${asList(aggregate?.map.get('needs')).length}`,
    `non-blocking jobs ${jobs.filter(({ job }) => NON_BLOCKING_JOBS.includes(job.id)).length}`,
    `aggregate payloads ${input.aggregateStatuses?.length ?? 0}`,
    ...extra,
  ];
  return `wiring self-check: ${parts.join(', ')}`;
};

export function checkWiring(input: WiringInput): WiringResult {
  const rows = parseList(input.listText);
  const callers = callersOf(input);
  const locked = checkLocked(rows, input.buildScript);
  const permissions = checkPermissions(input);
  const counts = countLine(input, rows, callers, [
    `cargo invocations ${locked.count}`,
    `token steps ${permissions.count}`,
  ]);
  const jobs = allJobs(input);
  const checkouts = jobs.flatMap(({ job }) => job.steps).filter(isCheckout).length;
  const unrecognised = input.unrecognised.map((reason) => ({
    condition: 'shape',
    message: `an unrecognised shape: ${reason}`,
  }));
  if (input.workflows.length + input.unrecognised.length === 0 || jobs.length === 0) {
    return {
      failures: [
        { condition: 'empty', message: 'no workflow file or no job was found' },
        ...unrecognised,
      ],
      counts,
    };
  }
  const ci = ciWorkflow(input);
  const failures: Failure[] = [
    ...(checkouts === 0 ? [{ condition: 'empty', message: 'no checkout step was found' }] : []),
    ...checkCalls(input),
    ...checkTags(rows, callers),
    ...checkAggregate(ci),
    ...checkJobIfs(input),
    ...checkJobShape(input),
    ...checkNoEscapes(input),
    ...checkCiTriggers(ci),
    ...checkPrText(input),
    ...checkColour(input, rows),
    ...checkNonBlockingRows(rows, callers),
    ...checkPayloads(input.aggregateStatuses),
    ...locked.failures,
    ...permissions.failures,
    ...checkBannedText(input, rows),
    ...unrecognised,
    ...checkNames(ci, input.requiredNames ?? REQUIRED_JOB_NAMES),
  ];
  if (rows.length === 0)
    failures.push({ condition: 'empty', message: 'the step table listed no row' });
  return { failures, counts };
}

// Every cargo call that reads the dependency graph, in a row's command or in
// scripts/build.sh, carries --locked in its own pipeline segment. cargo fmt
// resolves nothing and takes no --locked.
const GRAPH_SUBCOMMANDS = new Set([
  'build',
  'check',
  'clippy',
  'test',
  'doc',
  'deny',
  'llvm-cov',
  'metadata',
  'tree',
]);

const cargoCalls = (text: string): { readonly segment: string; readonly sub: string }[] =>
  text.split(/\|\||&&|[|;\n]/u).flatMap((segment) =>
    [...segment.matchAll(/(?:^|[^\w-])cargo(?:\s+xwin)?\s+([a-z-]+)/gu)]
      .map((found) => found[1] ?? '')
      .filter((sub) => GRAPH_SUBCOMMANDS.has(sub))
      .map((sub) => ({ segment, sub })),
  );

const checkLocked = (
  rows: readonly ListRow[],
  buildScript: string,
): { failures: Failure[]; count: number } => {
  const script = buildScript
    .replaceAll('\\\n', ' ')
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('#'))
    .join('\n');
  const sources = [
    ...rows.map((row) => ({ where: `the row ${row.name}`, text: row.cmd })),
    { where: 'scripts/build.sh', text: script },
  ];
  let count = 0;
  const failures: Failure[] = [];
  for (const source of sources) {
    for (const call of cargoCalls(source.text)) {
      count += 1;
      if (!/(?:^|\s)--locked(?:\s|$)/u.test(call.segment)) {
        failures.push({
          condition: 'locked',
          message: `${source.where}: cargo ${call.sub} runs without --locked`,
        });
      }
    }
  }
  if (count === 0)
    failures.push({
      condition: 'locked',
      message: 'no cargo call was found in the rows or scripts/build.sh',
    });
  return { failures, count };
};

// Permissions: {} at the top, contents: read for a job with a checkout and {}
// for one without, no write scope, persist-credentials: false on every
// checkout, no secrets. expression, and the job token only in the env of the
// runner call of the workflow-lint or freshness job.
const TOKEN_JOBS = ['workflows', 'freshness'];

const permissionText = (value: YamlValue | undefined): string => {
  if (value === undefined) return 'none';
  if (typeof value === 'string') return value;
  if (isMapping(value))
    return `{${[...value.entries()].map(([key, item]) => `${key}: ${typeof item === 'string' ? item : 'a nested value'}`).join(', ')}}`;
  return 'a list';
};

const checkJobPermissions = (workflow: Workflow, job: Job): Failure[] => {
  const where = `${baseName(workflow.file)} job ${job.id}`;
  const value = job.map.get('permissions');
  const wanted = job.steps.some(isCheckout) ? '{contents: read}' : '{}';
  const failures: Failure[] = [];
  const text = permissionText(value);
  if (text !== wanted)
    failures.push({
      condition: 'permissions',
      message: `${where} permissions are ${text}, not ${wanted}`,
    });
  if (/write|read-all/u.test(text))
    failures.push({ condition: 'permissions', message: `${where} permissions grant ${text}` });
  const kept = job.steps.filter(
    (step) =>
      isCheckout(step) &&
      asText(asMapping(step.get('with')).get('persist-credentials')) !== 'false',
  );
  if (kept.length > 0) {
    failures.push({
      condition: 'permissions',
      message: `${where} has ${kept.length} checkouts without persist-credentials: false`,
    });
  }
  return failures;
};

const checkTokens = (input: WiringInput): { failures: Failure[]; count: number } => {
  const failures: Failure[] = [];
  let count = 0;
  for (const workflow of input.workflows) {
    const where = baseName(workflow.file);
    if (mentions(workflow.root, 'secrets.'))
      failures.push({ condition: 'permissions', message: `${where} uses a secrets. expression` });
    const outside = [...workflow.root.entries()]
      .filter(([key]) => key !== 'jobs')
      .map(([, value]) => value);
    const jobLevel = workflow.jobs.map(
      (job) => new Map([...job.map.entries()].filter(([key]) => key !== 'steps')),
    );
    for (const value of [...outside, ...jobLevel]) {
      if (mentions(value, 'GH_TOKEN') || mentions(value, 'github.token')) {
        failures.push({
          condition: 'permissions',
          message: `${where} gives the job token outside a step`,
        });
      }
    }
    for (const job of workflow.jobs) {
      for (const step of job.steps.filter(
        (candidate) => mentions(candidate, 'GH_TOKEN') || mentions(candidate, 'github.token'),
      )) {
        const allowed =
          TOKEN_JOBS.includes(job.id) &&
          callTag(step) !== null &&
          !mentions(step.get('run'), 'token');
        if (allowed) count += 1;
        else
          failures.push({
            condition: 'permissions',
            message: `${where} job ${job.id} gives the job token to the step "${asText(step.get('name')) ?? ''}"`,
          });
      }
    }
  }
  return { failures, count };
};

const checkPermissions = (input: WiringInput): { failures: Failure[]; count: number } => {
  const failures = input.workflows.flatMap((workflow) => {
    const top = permissionText(workflow.root.get('permissions'));
    const topFailure =
      top === '{}'
        ? []
        : [
            {
              condition: 'permissions',
              message: `${baseName(workflow.file)} top-level permissions are ${top}, not {}`,
            },
          ];
    return [...topFailure, ...workflow.jobs.flatMap((job) => checkJobPermissions(workflow, job))];
  });
  const tokens = checkTokens(input);
  return { failures: [...failures, ...tokens.failures], count: tokens.count };
};

// Banned text. Each needle is written as parts joined at run time, so this
// file never matches its own scan.
const joined = (...parts: readonly string[]): string => parts.join('');
const FETCHING = [joined('np', 'x'), joined('node_modules/', '.bin'), joined('NODE_', 'OPTIONS')];
const ARTIFACT_UPLOAD = joined('upload-', 'artifact');
const CLAIMS = [
  joined('mir', 'ror'),
  joined('same ', 'checks'),
  joined('same as ', 'ci'),
  joined('pari', 'ty'),
  joined('safe to ', 'push'),
  joined('all ', 'green'),
  joined('shipped ', 'artifact'),
  joined('users actually ', 'download'),
];
const RETIRED = joined('mach', 'ete');
const NOT_ADOPTED = [
  joined('dependency-review-', 'action'),
  joined('cargo ', 'audit'),
  joined('osv-', 'scanner'),
  joined('lockfile-', 'lint'),
  joined('npm audit ', 'signatures'),
];

const isWorkflowFile = (file: string): boolean => /^\.github\/workflows\/[^/]+\.ya?ml$/u.test(file);

const bannedIn = (file: TrackedFile): { readonly needle: string; readonly what: string }[] => {
  const lower = file.text.toLowerCase();
  const hits: { needle: string; what: string }[] = [];
  const add = (needles: readonly string[], text: string, what: string): void => {
    for (const needle of needles) if (text.includes(needle)) hits.push({ needle, what });
  };
  if (file.path === 'scripts/ci-check.sh' || isWorkflowFile(file.path))
    add(FETCHING, file.text, 'a fetching or heap-flag front door');
  if (isWorkflowFile(file.path)) add([ARTIFACT_UPLOAD], file.text, 'an artifact upload');
  // The baselines are generated test ids, not prose; a test name may hold any word.
  const prose = !file.path.startsWith('scripts/test-baselines/');
  if (
    prose &&
    (file.path.startsWith('scripts/') ||
      file.path.startsWith('.github/') ||
      file.path === 'README.md')
  ) {
    add(CLAIMS, lower, 'a claim the gates do not make');
  }
  if (file.path !== 'CHANGELOG.md') add([RETIRED], lower, 'the retired unused-dependency tool');
  if (file.path.startsWith('scripts/') || isWorkflowFile(file.path))
    add(NOT_ADOPTED, lower, 'a tool that was not adopted');
  return hits;
};

const checkBannedText = (input: WiringInput, rows: readonly ListRow[]): Failure[] => {
  const files: TrackedFile[] = [
    ...input.trackedFiles,
    { path: 'scripts/ (the --list commands)', text: rows.map((row) => row.cmd).join('\n') },
  ];
  return files.flatMap((file) =>
    bannedIn(file).map((hit) => ({
      condition: 'banned text',
      message: `${file.path} holds ${hit.what} ("${hit.needle}")`,
    })),
  );
};

// Runs the aggregate job's run block the way the runner would, with one
// payload as NEEDS_JSON, and returns its exit status.
export async function runAggregateBlock(runBlock: string, payload: string): Promise<number> {
  const dir = mkdtempSync(path.join(tmpdir(), 'wiring-'));
  try {
    return await new Promise<number>((resolve) => {
      const child = spawn('bash', ['--noprofile', '--norc', '-eo', 'pipefail', '-c', runBlock], {
        env: {
          ...process.env,
          NEEDS_JSON: payload,
          GITHUB_STEP_SUMMARY: path.join(dir, 'summary.md'),
        },
        stdio: 'ignore',
      });
      child.on('error', () => {
        resolve(127);
      });
      child.on('close', (code) => {
        resolve(code ?? 1);
      });
    });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

export const aggregateBlock = (workflows: readonly Workflow[]): string | null => {
  const ci = workflows.find((workflow) => baseName(workflow.file) === 'ci.yml');
  const job = ci?.jobs.find((candidate) => candidate.id === AGGREGATE_JOB);
  return job?.steps.length === 1 ? asText(job.steps[0]?.get('run')) : null;
};

export const aggregateStatuses = async (runBlock: string | null): Promise<number[] | null> =>
  runBlock === null
    ? null
    : Promise.all(AGGREGATE_PAYLOADS.map((payload) => runAggregateBlock(runBlock, payload)));

const main = async (): Promise<number> => {
  const root = path.resolve(import.meta.dirname, '..', '..', '..');
  const dir = path.join(root, '.github', 'workflows');
  const workflows: Workflow[] = [];
  const unrecognised: string[] = [];
  for (const name of readdirSync(dir)
    .filter((file) => /\.ya?ml$/u.test(file))
    .sort()) {
    const file = `.github/workflows/${name}`;
    try {
      workflows.push(parseWorkflow(readFileSync(path.join(root, file), 'utf8'), file));
    } catch (error: unknown) {
      if (!(error instanceof UnrecognisedShape)) throw error;
      unrecognised.push(error.message);
    }
  }
  const listText = execFileSync('bash', ['scripts/ci-check.sh', '--list'], {
    cwd: root,
    encoding: 'utf8',
  });
  const statuses = await aggregateStatuses(aggregateBlock(workflows));
  const buildScript = readFileSync(path.join(root, 'scripts', 'build.sh'), 'utf8');
  const trackedFiles = execFileSync('git', ['ls-files', '-z'], { cwd: root, encoding: 'utf8' })
    .split('\0')
    .filter((file) => file !== '')
    .map((file) => ({ path: file, text: readFileSync(path.join(root, file), 'utf8') }));
  const result = checkWiring({
    workflows,
    unrecognised,
    listText,
    aggregateStatuses: statuses,
    buildScript,
    trackedFiles,
  });
  for (const failure of result.failures) {
    process.stdout.write(`wiring self-check: FAIL ${failure.message}\n`);
  }
  process.stdout.write(`${result.counts}\n`);
  return result.failures.length === 0 ? 0 : 1;
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  process.exitCode = await main();
}
