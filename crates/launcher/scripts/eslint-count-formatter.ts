// eslint's stylish report, unchanged, followed by how many files eslint linted
// per language and in total. A language that linted no file fails the run.
import { ESLint } from 'eslint';

const LANGUAGE: Record<string, string> = { svelte: 'svelte', ts: 'ts', js: 'js' };

export default async function format(
  results: ESLint.LintResult[],
  context: ESLint.LintResultData,
): Promise<string> {
  const stylish = await new ESLint().loadFormatter('stylish');
  const report = await stylish.format(results, context);
  const counts = new Map<string, number>([
    ['svelte', 0],
    ['ts', 0],
    ['js', 0],
  ]);
  for (const { filePath } of results) {
    const language = LANGUAGE[filePath.slice(filePath.lastIndexOf('.') + 1)];
    if (language === undefined) {
      throw new Error(`eslint linted ${filePath}, whose extension no count covers`);
    }
    counts.set(language, (counts.get(language) ?? 0) + 1);
  }
  const lines = [...counts].map(
    ([language, n]) => `eslint: ${String(n)} .${language} files linted`,
  );
  lines.push(`eslint: ${String(results.length)} files linted in total`);
  const empty = [...counts].filter(([, n]) => n === 0).map(([language]) => `.${language}`);
  if (empty.length > 0) {
    throw new Error(`eslint linted no ${empty.join(', ')} file`);
  }
  return `${report}${lines.join('\n')}\n`;
}
