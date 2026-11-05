// Self-test of the osv-scanner checker: literal osv reports against ignore
// files planted in a throwaway directory, each with the verdict it must get.
//
//   node crates/launcher/scripts/osv-scan.selftest.ts
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { check } from './osv-scan.ts';

interface Vuln {
  readonly id: string;
  readonly aliases?: readonly string[];
}

interface Pkg {
  readonly name: string;
  readonly vulns?: readonly Vuln[];
}

interface Case {
  readonly name: string;
  readonly report: string;
  readonly expect: number;
  // A line the checker must print.
  readonly says: string;
}

const RUST_ID = 'RUSTSEC-2024-0429';
const RUST_ALIAS = 'GHSA-wrw7-89jp-8q8g';
const NPM_ID = 'GHSA-gcqx-w48m-w2h4';

const root = mkdtempSync(path.join(tmpdir(), 'osv-scan-selftest-'));
mkdirSync(path.join(root, 'crates', 'launcher'), { recursive: true });
writeFileSync(
  path.join(root, 'deny.toml'),
  [
    '[graph]',
    'targets = []',
    '',
    '[advisories]',
    'version = 2',
    'ignore = [',
    `    { id = "${RUST_ID}", reason = "planted" },`,
    ']',
    '',
    '[licenses]',
    'allow = ["MIT"]',
    '',
  ].join('\n'),
);
writeFileSync(
  path.join(root, 'crates', 'launcher', 'npm-audit-ignore.json'),
  JSON.stringify([{ id: NPM_ID, package: 'merge-package', reason: 'planted' }]),
);

const CARGO = path.join(root, 'Cargo.lock');
const NPM = path.join(root, 'crates', 'launcher', 'package-lock.json');

const result = (file: string, packages: readonly Pkg[]): Record<string, unknown> => ({
  source: { path: file, type: 'lockfile' },
  packages: packages.map((pkg) => ({
    package: { name: pkg.name, version: '1.0.0', ecosystem: 'planted' },
    ...(pkg.vulns === undefined
      ? {}
      : { vulnerabilities: pkg.vulns.map((v) => ({ id: v.id, aliases: v.aliases ?? [] })) }),
  })),
});

// Both lockfiles, each with one clean package beside the planted ones.
const report = (cargo: readonly Pkg[], npm: readonly Pkg[]): string =>
  JSON.stringify({
    results: [
      result(CARGO, [{ name: 'clean-crate' }, ...cargo]),
      result(NPM, [{ name: 'clean-package' }, ...npm]),
    ],
  });

const cases: readonly Case[] = [
  {
    name: 'deny.toml id',
    report: report([{ name: 'unsound-crate', vulns: [{ id: RUST_ID }] }], []),
    expect: 0,
    says: 'findings 1 accepted 1 failing 0',
  },
  {
    name: 'deny.toml id as an alias',
    report: report(
      [{ name: 'unsound-crate', vulns: [{ id: RUST_ALIAS, aliases: [RUST_ID] }] }],
      [],
    ),
    expect: 0,
    says: 'findings 1 accepted 1 failing 0',
  },
  {
    name: 'npm id and package',
    report: report([], [{ name: 'merge-package', vulns: [{ id: NPM_ID }] }]),
    expect: 0,
    says: 'findings 1 accepted 1 failing 0',
  },
  {
    name: 'npm id on another package',
    report: report([], [{ name: 'other', vulns: [{ id: NPM_ID }] }]),
    expect: 1,
    says: `FAIL crates/launcher/package-lock.json other 1.0.0 ${NPM_ID}`,
  },
  {
    name: 'deny.toml id from package-lock.json',
    report: report([], [{ name: 'unsound-crate', vulns: [{ id: RUST_ID }] }]),
    expect: 1,
    says: `FAIL crates/launcher/package-lock.json unsound-crate 1.0.0 ${RUST_ID}`,
  },
  {
    name: 'npm id and package from Cargo.lock',
    report: report([{ name: 'merge-package', vulns: [{ id: NPM_ID }] }], []),
    expect: 1,
    says: `FAIL Cargo.lock merge-package 1.0.0 ${NPM_ID}`,
  },
  {
    name: 'unowned',
    report: report(
      [{ name: 'certs', vulns: [{ id: 'RUSTSEC-2026-0001', aliases: ['CVE-1'] }] }],
      [],
    ),
    expect: 1,
    says: 'FAIL Cargo.lock certs 1.0.0 RUSTSEC-2026-0001 CVE-1',
  },
  {
    name: 'zero packages',
    report: JSON.stringify({ results: [result(CARGO, []), result(NPM, [])] }),
    expect: 1,
    says: 'no package scanned',
  },
  {
    name: 'one lockfile',
    report: JSON.stringify({ results: [result(CARGO, [{ name: 'clean-crate' }])] }),
    expect: 1,
    says: 'scanned 1 lockfiles, not 2',
  },
  { name: 'malformed JSON', report: '{"results": [', expect: 1, says: 'not JSON' },
];

const failures: string[] = [];
try {
  for (const c of cases) {
    const outcome = check(c.report, root);
    const problems: string[] = [];
    if (outcome.code !== c.expect)
      problems.push(`exit ${String(outcome.code)}, expected ${String(c.expect)}`);
    if (!outcome.out.includes(c.says)) problems.push(`no output holding "${c.says}"`);
    if (problems.length > 0) failures.push(`${c.name}: ${problems.join('; ')}`);
  }
} finally {
  rmSync(root, { recursive: true, force: true });
}
for (const failure of failures)
  process.stdout.write(`osv scan self-test: not as expected: ${failure}\n`);
const expected = cases.length - failures.length;
process.stdout.write(
  `osv scan self-test: cases ${String(cases.length)}, as expected ${String(expected)}\n`,
);
process.exitCode = cases.length > 0 && expected === cases.length ? 0 : 1;
