// Self-test of the npm audit checker: literal reports and ignore lists, each
// with the verdict it must get.
//
//   node crates/launcher/scripts/npm-audit.selftest.ts
import { check } from './npm-audit.ts';

interface Case {
  readonly name: string;
  readonly report: string;
  readonly ignores: string;
  readonly expect: number;
  // A line the checker must print.
  readonly says: string;
}

const ID = 'GHSA-gcqx-w48m-w2h4';
const OTHER = 'GHSA-mrhx-c7xc-m3w5';

const advisory = (id: string, name: string): Record<string, unknown> => ({
  source: 1,
  name,
  dependency: name,
  title: `${name} advisory`,
  url: `https://github.com/advisories/${id}`,
  severity: 'low',
  range: '<9.9.9',
});

// A report in npm's audit v2 shape: `pkg` holds the advisory, `parent` only
// points at it.
const report = (vias: readonly Record<string, unknown>[], total = 190): string =>
  JSON.stringify({
    auditReportVersion: 2,
    vulnerabilities:
      vias.length === 0
        ? {}
        : {
            pkg: { name: 'pkg', severity: 'low', via: vias },
            parent: { name: 'parent', severity: 'low', via: ['pkg'] },
          },
    metadata: { dependencies: { total } },
  });

const entry = (id: string, pkg: string, reason = 'build-time only'): string =>
  JSON.stringify([{ id, package: pkg, reason }]);

const one = report([advisory(ID, 'pkg')]);

const cases: readonly Case[] = [
  { name: 'clean', report: report([]), ignores: '[]', expect: 0, says: 'advisories 0, ignored 0' },
  { name: 'ignored', report: one, ignores: entry(ID, 'pkg'), expect: 0, says: 'ignored 1' },
  { name: 'not ignored', report: one, ignores: '[]', expect: 1, says: `FAIL ${ID} pkg low` },
  {
    name: 'second advisory',
    report: report([advisory(ID, 'pkg'), advisory(OTHER, 'pkg')]),
    ignores: entry(ID, 'pkg'),
    expect: 1,
    says: `FAIL ${OTHER} pkg`,
  },
  {
    name: 'other package',
    report: one,
    ignores: entry(ID, 'other'),
    expect: 1,
    says: 'stale ignore',
  },
  { name: 'stale', report: report([]), ignores: entry(ID, 'pkg'), expect: 1, says: 'stale ignore' },
  { name: 'no reason', report: one, ignores: entry(ID, 'pkg', ' '), expect: 1, says: 'no reason' },
  { name: 'bad id', report: one, ignores: entry('CVE-1', 'pkg'), expect: 1, says: 'not a GHSA id' },
  {
    name: 'twice',
    report: one,
    ignores: JSON.stringify(
      [0, 1].map(() => ({ id: ID, package: 'pkg', reason: 'build-time only' })),
    ),
    expect: 1,
    says: 'listed twice',
  },
  { name: 'ignore not array', report: one, ignores: '{}', expect: 1, says: 'not a JSON array' },
  { name: 'no report', report: '', ignores: '[]', expect: 1, says: 'no JSON report' },
  {
    name: 'npm error',
    report: JSON.stringify({ error: { code: 'ENOAUDIT' } }),
    ignores: '[]',
    expect: 1,
    says: 'reported an error',
  },
  { name: 'no deps', report: report([], 0), ignores: '[]', expect: 1, says: 'no dependencies' },
  {
    name: 'url without id',
    report: report([{ ...advisory(ID, 'pkg'), url: 'https://example.com/x' }]),
    ignores: '[]',
    expect: 1,
    says: 'without a GHSA url',
  },
];

const failures: string[] = [];
for (const c of cases) {
  const result = check(c.report, c.ignores);
  const problems: string[] = [];
  if (result.code !== c.expect) problems.push(`exit ${result.code}, expected ${c.expect}`);
  if (!result.out.includes(c.says)) problems.push(`no output holding "${c.says}"`);
  if (problems.length > 0) failures.push(`${c.name}: ${problems.join('; ')}`);
}
for (const failure of failures)
  process.stdout.write(`npm audit self-test: not as expected: ${failure}\n`);
const expected = cases.length - failures.length;
process.stdout.write(`npm audit self-test: cases ${cases.length}, as expected ${expected}\n`);
process.exitCode = cases.length > 0 && expected === cases.length ? 0 : 1;
