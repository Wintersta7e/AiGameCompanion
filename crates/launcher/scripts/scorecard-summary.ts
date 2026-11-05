// Writes the OpenSSF Scorecard result as a Markdown table: the overall score,
// then each check's name, score and reason.
//
//   node crates/launcher/scripts/scorecard-summary.ts <results-file>
//
// Reads the JSON results file the Scorecard action writes and appends the
// table to $GITHUB_STEP_SUMMARY (stdout when it is unset). It reports and
// never gates on a score; it fails on a missing or malformed file, a result
// without an overall score, no check, or a check without a score.
// Exit status: 0 pass, 1 fail, 2 usage.
import { appendFileSync, readFileSync } from 'node:fs';
import path from 'node:path';

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

// A table cell: one line, its pipes escaped.
const cell = (value: unknown): string =>
  (typeof value === 'string' ? value : '').replaceAll(/\s+/gu, ' ').replaceAll('|', '\\|').trim();

const parse = (file: string): Record<string, unknown> | string => {
  let text: string;
  try {
    text = readFileSync(file, 'utf8');
  } catch (error) {
    return `the results file ${file} cannot be read: ${String(error)}`;
  }
  try {
    const value: unknown = JSON.parse(text);
    return isRecord(value) ? value : `the results file ${file} is not a JSON object`;
  } catch (error) {
    return `the results file ${file} is not JSON: ${String(error)}`;
  }
};

export const summarise = (file: string): { code: number; table: string; out: string } => {
  const failures: string[] = [];
  const result = parse(file);
  const fields = typeof result === 'string' ? {} : result;
  if (typeof result === 'string') failures.push(result);
  const score = typeof fields['score'] === 'number' ? fields['score'] : null;
  if (typeof result !== 'string' && score === null)
    failures.push('the results have no overall score');
  const checks = (Array.isArray(fields['checks']) ? fields['checks'] : []).map((item: unknown) =>
    isRecord(item) ? item : {},
  );
  if (typeof result !== 'string' && checks.length === 0) failures.push('the results list no check');
  const rows = checks.map((check) => {
    const value = check['score'];
    if (typeof value !== 'number') failures.push(`the check ${cell(check['name'])} has no score`);
    return `| ${cell(check['name'])} | ${typeof value === 'number' ? String(value) : '-'} | ${cell(check['reason'])} |`;
  });
  const table =
    failures.length > 0
      ? ''
      : [
          '### OpenSSF Scorecard',
          '',
          `Overall score: ${String(score)} / 10`,
          '',
          '| check | score | reason |',
          '|---|---|---|',
          ...rows,
          '',
        ].join('\n');
  const lines = [
    `scorecard summary: checks ${String(checks.length)} score ${score === null ? 'none' : String(score)}`,
    ...failures.map((line) => `scorecard summary: FAIL ${line}`),
  ];
  return { code: failures.length > 0 ? 1 : 0, table, out: `${lines.join('\n')}\n` };
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  const [file] = process.argv.slice(2);
  if (file === undefined || process.argv.length !== 3) {
    process.stderr.write('usage: scorecard-summary.ts <results-file>\n');
    process.exitCode = 2;
  } else {
    const result = summarise(path.resolve(file));
    const summary = process.env['GITHUB_STEP_SUMMARY'];
    if (summary === undefined || summary === '') process.stdout.write(result.table);
    else appendFileSync(summary, result.table);
    process.stdout.write(result.out);
    process.exitCode = result.code;
  }
}
