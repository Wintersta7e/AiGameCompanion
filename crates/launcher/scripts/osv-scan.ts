// Checks both lockfiles against OSV, with no ignore list of its own.
//
//   node crates/launcher/scripts/osv-scan.ts <repo-root>
//
// Runs osv-scanner over Cargo.lock and crates/launcher/package-lock.json and
// accepts a finding only where its owner already ignores it: a Cargo.lock
// finding when its id or an alias is an `id` in deny.toml [advisories] ignore,
// a package-lock.json finding when an npm-audit-ignore.json entry names its id
// (or an alias) and its package. Stale entries stay failed by their owners
// (cargo-deny, scripts/npm-audit.ts). An unowned Cargo.lock finding on a crate
// outside cargo-deny's graph (`cargo deny list`, the crates the deny.toml
// targets build) is printed as lock-only and does not fail: weak optional
// features keep crates in the lock that no build compiles, and cargo-deny
// rejects an ignore for a crate outside its graph. Fails on any other finding,
// on fewer than 2 lockfiles, on no scanned package, on a report without
// results and on a crate list that could not be read or is empty.
// A finding is one vulnerability entry of one package as osv reports it.
// Exit status: 0 pass, 1 fail, 2 usage.
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { type Ignore, parseIgnores } from './npm-audit.ts';

const CARGO_LOCK = 'Cargo.lock';
const NPM_LOCK = 'crates/launcher/package-lock.json';
const NPM_IGNORES = 'crates/launcher/npm-audit-ignore.json';
const LOCKFILES = 2;

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const text = (value: unknown): string => (typeof value === 'string' ? value : '');

const list = (value: unknown): readonly unknown[] => (Array.isArray(value) ? value : []);

// The `id` values of deny.toml's [advisories] ignore array, or null when the
// table or the array is missing.
const denyIgnoreIds = (toml: string): Set<string> | null => {
  const table = /^\[advisories\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/mu.exec(toml)?.[1];
  const array =
    table === undefined ? undefined : /^ignore\s*=\s*\[([\s\S]*?)^\]/mu.exec(table)?.[1];
  if (array === undefined) return null;
  return new Set([...array.matchAll(/\bid\s*=\s*"([^"]+)"/gu)].map((found) => found[1] ?? ''));
};

interface Finding {
  readonly lockfile: string;
  readonly name: string;
  readonly version: string;
  readonly id: string;
  readonly aliases: readonly string[];
}

interface Owners {
  readonly denyIds: ReadonlySet<string>;
  readonly npm: readonly Ignore[];
}

interface Scanned {
  readonly lockfile: string | null;
  readonly packages: number;
  readonly findings: readonly Finding[];
}

// One osv result: its lockfile (null for any other source), its package
// count and one finding per vulnerability entry of each package.
const scannedOf = (result: unknown, root: string): Scanned => {
  const source = isRecord(result) && isRecord(result['source']) ? result['source'] : {};
  const lockfile = path.relative(root, text(source['path'])).split(path.sep).join('/');
  if (lockfile !== CARGO_LOCK && lockfile !== NPM_LOCK)
    return { lockfile: null, packages: 0, findings: [] };
  const entries = list(isRecord(result) ? result['packages'] : undefined);
  const findings = entries.flatMap((entry) => {
    const pkg = isRecord(entry) && isRecord(entry['package']) ? entry['package'] : {};
    return list(isRecord(entry) ? entry['vulnerabilities'] : undefined).map((vuln) => ({
      lockfile,
      name: text(pkg['name']),
      version: text(pkg['version']),
      id: isRecord(vuln) ? text(vuln['id']) : '',
      aliases: isRecord(vuln) ? list(vuln['aliases']).map(text) : [],
    }));
  });
  return { lockfile, packages: entries.length, findings };
};

// Cargo.lock findings belong to deny.toml, package-lock.json findings to the
// npm ignore list, which binds each id to one package.
const owned = (finding: Finding, owners: Owners): boolean => {
  const ids = [finding.id, ...finding.aliases];
  return finding.lockfile === CARGO_LOCK
    ? ids.some((id) => owners.denyIds.has(id))
    : owners.npm.some((entry) => entry.package === finding.name && ids.includes(entry.id));
};

// The `<name> <version>` of each crate in `cargo deny list --format json`
// (keys `<name> <version> <source>`), or why the list is unusable.
export const builtCrates = (listJson: string): ReadonlySet<string> | string => {
  let parsed: unknown;
  try {
    parsed = JSON.parse(listJson);
  } catch (error) {
    return `the cargo-deny crate list is not JSON: ${String(error)}`;
  }
  if (!isRecord(parsed)) return 'the cargo-deny crate list is not an object';
  return new Set(Object.keys(parsed).map((key) => key.split(' ').slice(0, 2).join(' ')));
};

const reportResults = (osvJson: string): { results: readonly unknown[] } | string => {
  let report: unknown;
  try {
    report = JSON.parse(osvJson);
  } catch (error) {
    return `the osv-scanner report is not JSON: ${String(error)}`;
  }
  return isRecord(report) && Array.isArray(report['results'])
    ? { results: report['results'] }
    : 'the osv-scanner report has no results';
};

export const check = (
  osvJson: string,
  root: string,
  built: ReadonlySet<string> | string,
): { code: number; out: string } => {
  const failures: string[] = [];
  const notes: string[] = [];
  const counts = { lockfiles: 0, packages: 0, findings: 0, accepted: 0, lockOnly: 0 };
  if (typeof built === 'string') failures.push(built);
  else if (built.size === 0) failures.push('the cargo-deny crate list is empty');
  const parsed = reportResults(osvJson);
  if (typeof parsed === 'string') failures.push(parsed);
  else {
    const denyIds = denyIgnoreIds(readFileSync(path.join(root, 'deny.toml'), 'utf8'));
    if (denyIds === null) failures.push('deny.toml has no [advisories] ignore array');
    const npm = parseIgnores(readFileSync(path.join(root, NPM_IGNORES), 'utf8'));
    failures.push(...npm.problems.map((problem) => `${NPM_IGNORES}: ${problem}`));
    const owners = { denyIds: denyIds ?? new Set<string>(), npm: npm.ignores };
    for (const result of parsed.results) {
      const scanned = scannedOf(result, root);
      if (scanned.lockfile === null) {
        failures.push('a result from a source that is neither lockfile');
        continue;
      }
      counts.lockfiles += 1;
      counts.packages += scanned.packages;
      counts.findings += scanned.findings.length;
      for (const f of scanned.findings) {
        if (owned(f, owners)) counts.accepted += 1;
        else if (
          f.lockfile === CARGO_LOCK &&
          typeof built !== 'string' &&
          built.size > 0 &&
          !built.has(`${f.name} ${f.version}`)
        ) {
          counts.lockOnly += 1;
          notes.push(`lock-only (not built): ${f.name} ${f.version} ${f.id}`);
        } else
          failures.push(
            `${f.lockfile} ${f.name} ${f.version} ${f.id} ${f.aliases.join(',') || '-'}`,
          );
      }
    }
    if (counts.lockfiles !== LOCKFILES)
      failures.push(`scanned ${String(counts.lockfiles)} lockfiles, not ${String(LOCKFILES)}`);
    if (counts.packages === 0) failures.push('no package scanned');
  }
  const lines = [
    `osv scan: lockfiles ${String(counts.lockfiles)} packages ${String(counts.packages)} findings ${String(counts.findings)} accepted ${String(counts.accepted)} failing ${String(counts.findings - counts.accepted - counts.lockOnly)} lock-only ${String(counts.lockOnly)}`,
    ...notes.map((line) => `osv scan: ${line}`),
    ...failures.map((line) => `osv scan: FAIL ${line}`),
  ];
  return { code: failures.length > 0 ? 1 : 0, out: `${lines.join('\n')}\n` };
};

// cargo-deny reads the deny.toml [graph] targets itself.
const listCrates = (root: string): ReadonlySet<string> | string => {
  const run = spawnSync(
    'cargo',
    ['deny', '--locked', 'list', '--layout', 'crate', '--format', 'json'],
    {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'inherit'],
    },
  );
  if (run.error !== undefined) return `cargo deny list did not run: ${String(run.error)}`;
  if (run.status !== 0) return `cargo deny list exited ${String(run.status ?? run.signal)}`;
  return builtCrates(run.stdout);
};

// osv-scanner exits 1 when it reports a finding; the checker owns the verdict.
const scan = (root: string): { code: number; out: string } => {
  const run = spawnSync(
    'osv-scanner',
    [
      'scan',
      'source',
      '--lockfile',
      CARGO_LOCK,
      '--lockfile',
      NPM_LOCK,
      '--format',
      'json',
      '--all-packages',
    ],
    {
      cwd: root,
      encoding: 'utf8',
      maxBuffer: 256 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'inherit'],
    },
  );
  if (run.error !== undefined)
    return { code: 1, out: `osv scan: FAIL osv-scanner did not run: ${String(run.error)}\n` };
  if (run.status !== 0 && run.status !== 1)
    return {
      code: 1,
      out: `osv scan: FAIL osv-scanner exited ${String(run.status ?? run.signal)}\n`,
    };
  return check(run.stdout, root, listCrates(root));
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  const [root] = process.argv.slice(2);
  if (root === undefined || process.argv.length !== 3) {
    process.stderr.write('usage: osv-scan.ts <repo-root>\n');
    process.exitCode = 2;
  } else {
    const result = scan(path.resolve(root));
    process.stdout.write(result.out);
    process.exitCode = result.code;
  }
}
