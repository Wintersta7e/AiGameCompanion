// Self-test of the Scorecard summary: literal results files planted in a
// throwaway directory, each with the verdict and output it must get.
//
//   node crates/launcher/scripts/scorecard-summary.selftest.ts
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { summarise } from './scorecard-summary.ts';

interface Case {
  readonly name: string;
  // The results file's text; null plants no file.
  readonly results: string | null;
  readonly expect: number;
  // Lines the output must hold.
  readonly says: readonly string[];
}

const check = (name: string, score: number, reason: string): Record<string, unknown> => ({
  name,
  score,
  reason,
  details: null,
  documentation: { short: 'planted', url: 'https://example.invalid/' },
});

const CHECKS = [
  check('Binary-Artifacts', 10, 'no binaries found in the repo'),
  check('Code-Review', 0, 'Found 0/30 approved changesets | score normalized to 0'),
  check('Packaging', -1, 'packaging workflow not detected'),
];

const results = (fields: Record<string, unknown>): string =>
  JSON.stringify({
    date: '2026-11-02T03:23:00Z',
    repo: { name: 'example.invalid/owner/repo', commit: '0'.repeat(40) },
    scorecard: { version: 'v5.0.0', commit: '1'.repeat(40) },
    ...fields,
    metadata: null,
  });

const cases: readonly Case[] = [
  {
    name: 'a full result',
    results: results({ score: 6.7, checks: CHECKS }),
    expect: 0,
    says: [
      'scorecard summary: checks 3 score 6.7',
      'Overall score: 6.7 / 10',
      '| Binary-Artifacts | 10 | no binaries found in the repo |',
      '| Code-Review | 0 | Found 0/30 approved changesets \\| score normalized to 0 |',
      '| Packaging | -1 | packaging workflow not detected |',
    ],
  },
  {
    name: 'a missing results file',
    results: null,
    expect: 1,
    says: ['scorecard summary: checks 0 score none', 'FAIL the results file'],
  },
  {
    name: 'no overall score',
    results: results({ checks: CHECKS }),
    expect: 1,
    says: ['scorecard summary: checks 3 score none', 'FAIL the results have no overall score'],
  },
  {
    name: 'zero checks',
    results: results({ score: 5, checks: [] }),
    expect: 1,
    says: ['scorecard summary: checks 0 score 5', 'FAIL the results list no check'],
  },
  {
    name: 'a check without a score',
    results: results({ score: 5, checks: [{ name: 'Fuzzing', reason: 'planted' }] }),
    expect: 1,
    says: ['FAIL the check Fuzzing has no score'],
  },
  {
    name: 'a malformed results file',
    results: '{"score": ',
    expect: 1,
    says: ['is not JSON'],
  },
];

const dir = mkdtempSync(path.join(tmpdir(), 'scorecard-summary-selftest-'));
const failures: string[] = [];
try {
  for (const [index, c] of cases.entries()) {
    const file = path.join(dir, `results-${String(index)}.json`);
    if (c.results !== null) writeFileSync(file, c.results);
    const outcome = summarise(file);
    const out = `${outcome.table}${outcome.out}`;
    const problems: string[] = [];
    if (outcome.code !== c.expect)
      problems.push(`exit ${String(outcome.code)}, expected ${String(c.expect)}`);
    for (const line of c.says)
      if (!out.includes(line)) problems.push(`no output holding "${line}"`);
    if (problems.length > 0) failures.push(`${c.name}: ${problems.join('; ')}`);
  }
} finally {
  rmSync(dir, { recursive: true, force: true });
}
for (const failure of failures)
  process.stdout.write(`scorecard summary self-test: not as expected: ${failure}\n`);
const expected = cases.length - failures.length;
process.stdout.write(
  `scorecard summary self-test: cases ${String(cases.length)}, as expected ${String(expected)}\n`,
);
process.exitCode = cases.length > 0 && expected === cases.length ? 0 : 1;
