// Checks `npm audit --json` against the reasoned ignore list.
//
//   npm audit --json | node scripts/npm-audit.ts <ignore-file>
//
// Fails on any advisory the list does not name, on a list entry that matches
// no current advisory (stale: the fix shipped, so the entry must go), on a
// malformed entry, and on an audit that resolved no dependencies. An entry
// covers one advisory id for one package. Exit status: 0 pass, 1 fail, 2 usage.
import { readFileSync } from 'node:fs';
import path from 'node:path';

export interface Ignore {
  readonly id: string;
  readonly package: string;
  readonly reason: string;
}

interface Advisory {
  readonly id: string;
  readonly package: string;
  readonly severity: string;
  readonly title: string;
}

const GHSA = /^GHSA(?:-[23456789cfghjmpqrvwx]{4}){3}$/u;

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const text = (value: unknown): string => (typeof value === 'string' ? value : '');

export const parseIgnores = (raw: string): { ignores: Ignore[]; problems: string[] } => {
  const problems: string[] = [];
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch (error) {
    return { ignores: [], problems: [`ignore file is not JSON: ${String(error)}`] };
  }
  if (!Array.isArray(data)) return { ignores: [], problems: ['ignore file is not a JSON array'] };
  const ignores: Ignore[] = [];
  const seen = new Set<string>();
  data.forEach((entry: unknown, i: number) => {
    const id = isRecord(entry) ? text(entry['id']) : '';
    const pkg = isRecord(entry) ? text(entry['package']) : '';
    const reason = isRecord(entry) ? text(entry['reason']).trim() : '';
    if (!GHSA.test(id)) problems.push(`entry ${i}: id ${JSON.stringify(id)} is not a GHSA id`);
    else if (pkg === '') problems.push(`entry ${i} (${id}): no package`);
    else if (reason === '') problems.push(`entry ${i} (${id}): no reason`);
    else if (seen.has(`${id} ${pkg}`)) problems.push(`entry ${i} (${id}): listed twice`);
    else {
      seen.add(`${id} ${pkg}`);
      ignores.push({ id, package: pkg, reason });
    }
  });
  return { ignores, problems };
};

// Every advisory in the report: the objects in each package's `via` list. The
// string entries only point at another package's advisories.
const advisoriesOf = (
  report: Record<string, unknown>,
): { advisories: Advisory[]; problems: string[] } => {
  const problems: string[] = [];
  const found = new Map<string, Advisory>();
  const vulns = report['vulnerabilities'];
  if (!isRecord(vulns)) return { advisories: [], problems: ['report has no vulnerabilities map'] };
  for (const [name, vuln] of Object.entries(vulns)) {
    const via = isRecord(vuln) ? vuln['via'] : undefined;
    if (!Array.isArray(via)) {
      problems.push(`${name}: no via list`);
      continue;
    }
    for (const item of via) {
      if (!isRecord(item)) continue;
      const id = /GHSA(?:-[a-z0-9]{4}){3}$/u.exec(text(item['url']))?.[0];
      if (id === undefined) {
        problems.push(`${name}: advisory without a GHSA url (${text(item['url']) || 'none'})`);
        continue;
      }
      const pkg = text(item['name']) || name;
      found.set(`${id} ${pkg}`, {
        id,
        package: pkg,
        severity: text(item['severity']),
        title: text(item['title']),
      });
    }
  }
  return { advisories: [...found.values()], problems };
};

export const check = (auditJson: string, ignoreText: string): { code: number; out: string } => {
  const lines: string[] = [];
  const fail = (line: string): void => {
    lines.push(`npm audit: FAIL ${line}`);
  };
  let report: unknown;
  try {
    report = JSON.parse(auditJson);
  } catch {
    report = undefined;
  }
  if (!isRecord(report)) {
    fail('npm audit printed no JSON report');
    lines.push('npm audit: dependencies audited: none');
    return { code: 1, out: `${lines.join('\n')}\n` };
  }
  if (report['error'] !== undefined)
    fail(`npm audit reported an error: ${JSON.stringify(report['error'])}`);
  const parsed = parseIgnores(ignoreText);
  for (const problem of parsed.problems) fail(problem);
  const { advisories, problems } = advisoriesOf(report);
  for (const problem of problems) fail(problem);

  const ignored = new Map(parsed.ignores.map((entry) => [`${entry.id} ${entry.package}`, entry]));
  let ignoredCount = 0;
  for (const adv of advisories) {
    const key = `${adv.id} ${adv.package}`;
    const entry = ignored.get(key);
    if (entry === undefined) {
      fail(`${adv.id} ${adv.package} ${adv.severity}: ${adv.title}`);
    } else {
      ignoredCount += 1;
      ignored.delete(key);
      lines.push(`npm audit: ignored ${adv.id} ${adv.package} ${adv.severity}: ${entry.reason}`);
    }
  }
  for (const entry of ignored.values())
    fail(`stale ignore ${entry.id} ${entry.package}: no current advisory; remove the entry`);

  const meta = isRecord(report['metadata']) ? report['metadata'] : {};
  const deps = isRecord(meta['dependencies']) ? meta['dependencies']['total'] : undefined;
  const total = typeof deps === 'number' && Number.isInteger(deps) && deps > 0 ? deps : null;
  if (total === null) fail('the report resolved no dependencies');
  lines.push(`npm audit: advisories ${advisories.length}, ignored ${ignoredCount}`);
  lines.push(`npm audit: dependencies audited: ${total ?? 'none'}`);
  const failed = lines.some((line) => line.startsWith('npm audit: FAIL '));
  return { code: failed ? 1 : 0, out: `${lines.join('\n')}\n` };
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  const [ignoreFile] = process.argv.slice(2);
  if (ignoreFile === undefined || process.argv.length !== 3) {
    process.stderr.write('usage: npm audit --json | npm-audit.ts <ignore-file>\n');
    process.exitCode = 2;
  } else {
    const result = check(readFileSync(0, 'utf8'), readFileSync(ignoreFile, 'utf8'));
    process.stdout.write(result.out);
    process.exitCode = result.code;
  }
}
