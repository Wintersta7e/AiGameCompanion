// Lists each pin beside the newest upstream release and its date: the Rust
// toolchain, Node, each tool version in scripts/ci-tools.env and each runner
// label. The SDK and CRT lines are printed as not checked. A row whose query
// fails prints why and is not counted; the report fails only when it counted
// nothing.
//
//   node crates/launcher/scripts/freshness.ts
//
// GH_TOKEN, when set, is sent to api.github.com only. When GITHUB_STEP_SUMMARY
// is set, the table and the count line are appended to it too.
import { appendFileSync, readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';

export interface ToolSource {
  readonly key: string;
  readonly repo: string;
  // The tool's install-action manifest; null for a tool installed by curl
  // with a checksum.
  readonly manifest: string | null;
}

// Where each tool version of scripts/ci-tools.env is released. The pin guard
// checks that this list and the tool file hold the same keys.
export const TOOL_SOURCES: readonly ToolSource[] = [
  { key: 'CARGO_XWIN', repo: 'rust-cross/cargo-xwin', manifest: 'cargo-xwin' },
  { key: 'CARGO_DENY', repo: 'EmbarkStudios/cargo-deny', manifest: 'cargo-deny' },
  { key: 'CARGO_LLVM_COV', repo: 'taiki-e/cargo-llvm-cov', manifest: 'cargo-llvm-cov' },
  { key: 'ZIZMOR', repo: 'zizmorcore/zizmor', manifest: 'zizmor' },
  { key: 'TYPOS', repo: 'crate-ci/typos', manifest: 'typos' },
  { key: 'GITLEAKS', repo: 'gitleaks/gitleaks', manifest: null },
  { key: 'ACTIONLINT', repo: 'rhysd/actionlint', manifest: null },
];

// Printed, never queried: they move only with a deliberate review.
const NOT_CHECKED = ['XWIN_SDK_VERSION', 'XWIN_CRT_VERSION'];

const RUST_CHANNEL = 'https://static.rust-lang.org/dist/channel-rust-stable.toml';
const NODE_INDEX = 'https://nodejs.org/dist/index.json';
const NODE_SCHEDULE = 'https://raw.githubusercontent.com/nodejs/Release/main/schedule.json';
const RUNNER_IMAGES = 'https://raw.githubusercontent.com/actions/runner-images/main/README.md';
const TIMEOUT_MS = 20_000;
const DAY_MS = 86_400_000;
const COOLING_DAYS = 7;

interface Row {
  readonly pin: string;
  readonly pinned: string;
  readonly newest: string;
  readonly released: string;
  readonly behind: string;
  readonly note: string;
  // true when every query the row needs answered and parsed; null for a pin
  // that is printed but not queried.
  readonly counted: boolean | null;
}

// A failed read or query; its message is the row's reason.
class Unanswered extends Error {}

const root = path.resolve(import.meta.dirname, '..', '..', '..');

const errorCode = (error: unknown): string => {
  if (error instanceof Error && 'code' in error && typeof error.code === 'string')
    return error.code;
  if (error instanceof Error && error.name === 'TimeoutError') return 'timed out';
  if (error instanceof Error && error.cause !== undefined) return errorCode(error.cause);
  return error instanceof Error ? error.message : String(error);
};

// An absent file and an unreadable one give different reasons.
const readRepoFile = (file: string): string => {
  try {
    return readFileSync(path.join(root, file), 'utf8');
  } catch (error: unknown) {
    const code = errorCode(error);
    throw new Unanswered(
      code === 'ENOENT' ? `${file} is missing` : `${file} is unreadable (${code})`,
    );
  }
};

const fetchText = async (url: string): Promise<string> => {
  const target = new URL(url);
  const headers: Record<string, string> = { 'User-Agent': 'pin-freshness-report' };
  const token = process.env['GH_TOKEN'] ?? '';
  if (target.hostname === 'api.github.com') {
    headers['Accept'] = 'application/vnd.github+json';
    if (token !== '') headers['Authorization'] = `Bearer ${token}`;
  }
  let response: Response;
  try {
    response = await fetch(target, { headers, signal: AbortSignal.timeout(TIMEOUT_MS) });
  } catch (error: unknown) {
    throw new Unanswered(`${target.hostname} did not answer (${errorCode(error)})`);
  }
  if (!response.ok)
    throw new Unanswered(
      `${target.hostname}${target.pathname} answered ${String(response.status)}`,
    );
  return response.text();
};

const isRecord = (value: unknown): value is Readonly<Record<string, unknown>> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const fetchJson = async (url: string): Promise<unknown> => {
  const text = await fetchText(url);
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new Unanswered(`${new URL(url).hostname} did not send JSON`);
  }
};

const daysBetween = (from: string, to: string): number =>
  Math.floor((Date.parse(`${to}T00:00:00Z`) - Date.parse(`${from}T00:00:00Z`)) / DAY_MS);

const compared = (
  pin: string,
  pinned: string,
  newest: string,
  released: string,
  today: string,
  note: string,
): Row => {
  const age = daysBetween(released, today);
  const notes = [note, age < COOLING_DAYS ? `cooling (released ${String(age)} days ago)` : ''];
  return {
    pin,
    pinned,
    newest,
    released,
    behind: pinned === newest ? '0' : String(age),
    note: notes.filter((part) => part !== '').join('; '),
    counted: true,
  };
};

const failed = (pin: string, pinned: string, error: unknown): Row => {
  if (!(error instanceof Unanswered)) throw error;
  const note = `failed: ${error.message}`;
  return { pin, pinned, newest: '-', released: '-', behind: '-', note, counted: false };
};

const DATE = /^\d{4}-\d{2}-\d{2}$/u;
const VERSION = /^\d+\.\d+\.\d+$/u;

const rustRow = async (today: string): Promise<Row> => {
  let pinned = '-';
  try {
    const channel = /^channel\s*=\s*"([^"]+)"/mu.exec(readRepoFile('rust-toolchain.toml'))?.[1];
    if (channel === undefined) throw new Unanswered('rust-toolchain.toml has no channel line');
    pinned = channel;
    const text = await fetchText(RUST_CHANNEL);
    // The file's first version line is cargo's; rustc's is under [pkg.rustc].
    const start = text.indexOf('\n[pkg.rustc]\n');
    const rustc = start < 0 ? '' : (text.slice(start + 1).split(/\n\[/u)[0] ?? '');
    const newest = /^version = "(\S+)/mu.exec(rustc)?.[1] ?? '';
    const date = /^date = "([^"]+)"/mu.exec(text.split(/\n\[/u)[0] ?? '')?.[1] ?? '';
    if (!VERSION.test(newest) || !DATE.test(date))
      throw new Unanswered('the stable channel file has no rustc version or date');
    return compared('Rust', pinned, newest, date, today, '');
  } catch (error: unknown) {
    return failed('Rust', pinned, error);
  }
};

const nodeRow = async (today: string): Promise<Row> => {
  let pinned = '-';
  try {
    pinned = readRepoFile('.nvmrc').trim();
    const major = /^v?(\d+)/u.exec(pinned)?.[1];
    if (major === undefined) throw new Unanswered('.nvmrc holds no version');
    const [index, schedule] = await Promise.all([fetchJson(NODE_INDEX), fetchJson(NODE_SCHEDULE)]);
    const entry = (Array.isArray(index) ? (index as unknown[]) : [])
      .filter(isRecord)
      .find(
        (item) => typeof item['version'] === 'string' && item['version'].startsWith(`v${major}.`),
      );
    const newest = typeof entry?.['version'] === 'string' ? entry['version'].slice(1) : '';
    const date = typeof entry?.['date'] === 'string' ? entry['date'] : '';
    const line = isRecord(schedule) ? schedule[`v${major}`] : undefined;
    const end = isRecord(line) && typeof line['end'] === 'string' ? line['end'] : '';
    if (!VERSION.test(newest) || !DATE.test(date) || !DATE.test(end))
      throw new Unanswered(`nodejs.org lists no v${major} release or end-of-life date`);
    const note = `end of life ${end}, in ${String(daysBetween(today, end))} days`;
    return compared('Node', pinned, newest, date, today, note);
  } catch (error: unknown) {
    return failed('Node', pinned, error);
  }
};

const toolFile = (): ReadonlyMap<string, string> =>
  new Map(
    readRepoFile('scripts/ci-tools.env')
      .split('\n')
      .map((line) => /^([A-Z][A-Z0-9_]*)=(\S+)$/u.exec(line.trim()))
      .filter((found) => found !== null)
      .map((found) => [found[1] ?? '', found[2] ?? '']),
  );

const installActionCommit = (): string => {
  const commit = /uses: taiki-e\/install-action@([0-9a-f]{40})/u.exec(
    readRepoFile('.github/workflows/ci.yml'),
  )?.[1];
  if (commit === undefined) throw new Unanswered('ci.yml pins no install-action commit');
  return commit;
};

const manifestNote = async (source: ToolSource, newest: string): Promise<string> => {
  if (source.manifest === null) return 'installed by curl with a checksum';
  const manifest = await fetchJson(
    `https://raw.githubusercontent.com/taiki-e/install-action/${installActionCommit()}/manifests/${source.manifest}.json`,
  );
  if (!isRecord(manifest)) throw new Unanswered(`the ${source.manifest} manifest is not an object`);
  return Object.hasOwn(manifest, newest)
    ? 'in the pinned install-action manifest'
    : 'not in the pinned install-action manifest';
};

const toolRow = async (source: ToolSource, today: string): Promise<Row> => {
  let pinned = '-';
  try {
    pinned = toolFile().get(source.key) ?? '';
    if (pinned === '') throw new Unanswered(`scripts/ci-tools.env has no ${source.key}`);
    const release = await fetchJson(`https://api.github.com/repos/${source.repo}/releases/latest`);
    const tag =
      isRecord(release) && typeof release['tag_name'] === 'string' ? release['tag_name'] : '';
    const published = isRecord(release) ? release['published_at'] : undefined;
    const newest = tag.replace(/^v/u, '');
    const date = typeof published === 'string' ? published.slice(0, 10) : '';
    if (newest === '' || !DATE.test(date))
      throw new Unanswered(`${source.repo} has no latest release with a date`);
    const note = await manifestNote(source, newest);
    return compared(source.key, pinned, newest, date, today, note);
  } catch (error: unknown) {
    return failed(source.key, pinned, error);
  }
};

const notCheckedRows = (): Row[] => {
  let values: ReadonlyMap<string, string> = new Map();
  try {
    values = toolFile();
  } catch (error: unknown) {
    if (!(error instanceof Unanswered)) throw error;
  }
  return NOT_CHECKED.map((key) => ({
    pin: key,
    pinned: values.get(key) ?? '-',
    newest: '-',
    released: '-',
    behind: '-',
    note: 'not checked',
    counted: null,
  }));
};

// Every runs-on value of every workflow file, once each.
const runnerLabels = (): string[] => {
  const dir = '.github/workflows';
  let names: string[];
  try {
    names = readdirSync(path.join(root, dir)).filter((name) => /\.ya?ml$/u.test(name));
  } catch (error: unknown) {
    throw new Unanswered(`${dir} is unreadable (${errorCode(error)})`);
  }
  const labels = names.flatMap((name) =>
    [...readRepoFile(`${dir}/${name}`).matchAll(/^\s*runs-on:\s*(\S+)/gmu)].map(
      (found) => found[1] ?? '',
    ),
  );
  return [...new Set(labels)].sort((a, b) => a.localeCompare(b));
};

// The image table: labels are backticked in the third column, a deprecated
// image carries a badge in the first.
const listedLabels = (readme: string): ReadonlyMap<string, boolean> => {
  const listed = new Map<string, boolean>();
  for (const line of readme.split('\n').filter((text) => text.startsWith('|'))) {
    const cells = line.split('|');
    const deprecated = /deprecated/iu.test(cells[1] ?? '');
    for (const found of (cells[3] ?? '').matchAll(/`([^`]+)`/gu)) {
      listed.set(found[1] ?? '', deprecated);
    }
  }
  if (listed.size === 0) throw new Unanswered('the runner-images README lists no label');
  return listed;
};

const numbers = (label: string): number[] => [...label.matchAll(/\d+/gu)].map((n) => Number(n[0]));

// The highest listed label of the same shape, digits compared as numbers.
const newestOfShape = (label: string, listed: ReadonlyMap<string, boolean>): string => {
  const shape = label.replaceAll(/\d+/gu, '#');
  const newer = (a: string, b: string): boolean => {
    const x = numbers(a);
    const y = numbers(b);
    const i = x.findIndex((n, k) => n !== y[k]);
    return i >= 0 && (x[i] ?? 0) > (y[i] ?? 0);
  };
  return [...listed.keys()]
    .filter((candidate) => candidate.replaceAll(/\d+/gu, '#') === shape)
    .reduce((best, candidate) => (best === '-' || newer(candidate, best) ? candidate : best), '-');
};

const runnerRows = async (): Promise<Row[]> => {
  let labels: string[];
  try {
    labels = runnerLabels();
  } catch (error: unknown) {
    return [failed('runner labels', '-', error)];
  }
  let listed: ReadonlyMap<string, boolean>;
  try {
    listed = listedLabels(await fetchText(RUNNER_IMAGES));
  } catch (error: unknown) {
    return labels.map((label) => failed('runner label', label, error));
  }
  return labels.map((label) => {
    const deprecated = listed.get(label);
    const note =
      deprecated === undefined ? 'not listed' : deprecated ? 'listed, deprecated' : 'listed';
    const newest = newestOfShape(label, listed);
    return {
      pin: 'runner label',
      pinned: label,
      newest,
      released: 'n/a',
      behind: 'n/a',
      note,
      counted: true,
    };
  });
};

const cell = (text: string, html: boolean): string => {
  const escaped = html
    ? text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    : text;
  return escaped.replaceAll('|', '\\|');
};

const table = (rows: readonly Row[], html: boolean): string =>
  [
    '| pin | pinned | newest | released | days behind | note |',
    '|---|---|---|---|---|---|',
    ...rows.map(
      (row) =>
        `| ${[row.pin, row.pinned, row.newest, row.released, row.behind, row.note || '-']
          .map((text) => cell(text, html))
          .join(' | ')} |`,
    ),
  ].join('\n');

const main = async (): Promise<number> => {
  const today = new Date().toISOString().slice(0, 10);
  const authenticated = (process.env['GH_TOKEN'] ?? '') !== '';
  process.stdout.write(
    `freshness: GitHub API requests authenticated: ${authenticated ? 'yes' : 'no'}\n`,
  );
  const [rust, node, tools, runners] = await Promise.all([
    rustRow(today),
    nodeRow(today),
    Promise.all(TOOL_SOURCES.map((source) => toolRow(source, today))),
    runnerRows(),
  ]);
  const rows = [rust, node, ...tools, ...notCheckedRows(), ...runners];
  const queried = rows.filter((row) => row.counted !== null).length;
  const counted = rows.filter((row) => row.counted === true).length;
  const line = `checked ${String(counted)} of ${String(queried)} pins on ${today}`;
  process.stdout.write(`${table(rows, false)}\n${line}\n`);
  const summary = process.env['GITHUB_STEP_SUMMARY'] ?? '';
  if (summary !== '') appendFileSync(summary, `\n${table(rows, true)}\n\n${line}\n`);
  return counted > 0 ? 0 : 1;
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  process.exitCode = await main();
}
