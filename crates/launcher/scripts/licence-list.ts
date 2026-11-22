// Checks that the dependency review's licence list restates deny.toml.
//
//   node crates/launcher/scripts/licence-list.ts <repo-root>
//
// deny.toml [licenses] allow owns the Rust licences; the dependency review
// cannot read it, so .github/dependency-review-config.yml lists them again
// beside the licences only npm packages use. Fails when a deny.toml licence is
// missing from the config, when a config licence outside deny.toml is used by
// no package in crates/launcher/package-lock.json (its `license` expressions
// split on OR, AND and parentheses), and when the lockfile lists no package.
// The config's allow-dependencies-licenses exempts lock-only crates (in
// Cargo.lock, compiled by no build of the deny.toml targets) from the review's
// licence check; fails when an exempted entry is not a versionless cargo purl,
// is not in Cargo.lock or is in `cargo deny list` (built, so deny.toml owns its
// licence), and when that crate list could not be read or is empty.
// Exit status: 0 pass, 1 fail, 2 usage.
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { listCrates } from './osv-scan.ts';

const DENY = 'deny.toml';
const CONFIG = '.github/dependency-review-config.yml';
const NPM_LOCK = 'crates/launcher/package-lock.json';
const CARGO_LOCK = 'Cargo.lock';

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

// The strings of deny.toml's [licenses] allow array, or null when the table
// or the array is missing.
const denyAllowed = (toml: string): string[] | null => {
  const table = /^\[licenses\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/mu.exec(toml)?.[1];
  const array = table === undefined ? undefined : /^allow\s*=\s*\[([\s\S]*?)\]/mu.exec(table)?.[1];
  if (array === undefined) return null;
  return [...array.matchAll(/"([^"]+)"/gu)].map((found) => found[1] ?? '');
};

// The items of the config's top-level block list under `key`, or null when
// the key is missing.
const configList = (yaml: string, key: string): string[] | null => {
  const lines = yaml.split('\n');
  const start = lines.findIndex((line) => line.trimEnd() === `${key}:`);
  if (start === -1) return null;
  const items: string[] = [];
  for (const line of lines.slice(start + 1)) {
    const item = /^ {2}- (.+?)\s*$/u.exec(line)?.[1];
    if (item === undefined) break;
    items.push(item);
  }
  return items;
};

// Each non-root package's licence identifiers, or a reason the text is not
// a lockfile.
const npmLicences = (lockJson: string): { packages: number; used: Set<string> } | string => {
  let lock: unknown;
  try {
    lock = JSON.parse(lockJson);
  } catch (error) {
    return `${NPM_LOCK} is not JSON: ${String(error)}`;
  }
  const packages = isRecord(lock) && isRecord(lock['packages']) ? lock['packages'] : {};
  const entries = Object.entries(packages).filter(([key]) => key !== '');
  const used = new Set<string>();
  for (const [, entry] of entries) {
    const licence = isRecord(entry) ? entry['license'] : undefined;
    if (typeof licence !== 'string') continue;
    for (const id of licence.split(/[()]|\s+(?:OR|AND)\s+/u)) {
      if (id.trim() !== '') used.add(id.trim());
    }
  }
  return { packages: entries.length, used };
};

// The package names in Cargo.lock.
const cargoNames = (lockToml: string): Set<string> =>
  new Set([...lockToml.matchAll(/^name = "([^"]+)"$/gmu)].map((found) => found[1] ?? ''));

// Each allow-dependencies-licenses entry that is not a lock-only crate.
const exemptionProblems = (
  exempted: readonly string[],
  cargoLock: string,
  built: ReadonlySet<string> | string,
): string[] => {
  if (exempted.length === 0) return [];
  if (typeof built === 'string') return [built];
  if (built.size === 0) return ['the cargo-deny crate list is empty'];
  const locked = cargoNames(cargoLock);
  const builtNames = new Set([...built].map((key) => key.split(' ')[0] ?? ''));
  return exempted.flatMap((purl) => {
    const name = /^pkg:cargo\/([A-Za-z0-9_-]+)$/u.exec(purl)?.[1];
    if (name === undefined) return [`exempted package ${purl} is not a versionless cargo purl`];
    if (!locked.has(name)) return [`exempted crate ${name} is not in Cargo.lock`];
    if (builtNames.has(name)) return [`exempted crate ${name} is built`];
    return [];
  });
};

export const check = (
  denyToml: string,
  configYaml: string,
  lockJson: string,
  cargoLock: string,
  built: ReadonlySet<string> | string,
): { code: number; out: string } => {
  const failures: string[] = [];
  const deny = denyAllowed(denyToml);
  if (deny === null) failures.push('deny.toml has no [licenses] allow array');
  const config = configList(configYaml, 'allow-licenses');
  if (config === null || config.length === 0)
    failures.push('the review config has no allow-licenses list');
  const npm = npmLicences(lockJson);
  if (typeof npm === 'string') failures.push(npm);
  else if (npm.packages === 0) failures.push('the npm lockfile lists no package');
  const denySet = new Set(deny ?? []);
  const configSet = new Set(config ?? []);
  for (const licence of denySet) {
    if (!configSet.has(licence))
      failures.push(`the deny.toml licence ${licence} is not in the review config`);
  }
  const npmOnly = [...configSet].filter((licence) => !denySet.has(licence));
  const used = typeof npm === 'string' ? new Set<string>() : npm.used;
  for (const licence of npmOnly) {
    if (!used.has(licence))
      failures.push(
        `the review config allows ${licence}, which is not in deny.toml and no npm package uses`,
      );
  }
  const exempted = configList(configYaml, 'allow-dependencies-licenses') ?? [];
  failures.push(...exemptionProblems(exempted, cargoLock, built));
  const packages = typeof npm === 'string' ? 0 : npm.packages;
  const lines = [
    `licence list: deny ${String(denySet.size)} config ${String(configSet.size)} npm-only ${String(npmOnly.length)} npm packages ${String(packages)} exempt ${String(exempted.length)}`,
    ...failures.map((line) => `licence list: FAIL ${line}`),
  ];
  return { code: failures.length > 0 ? 1 : 0, out: `${lines.join('\n')}\n` };
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  const [root] = process.argv.slice(2);
  if (root === undefined || process.argv.length !== 3) {
    process.stderr.write('usage: licence-list.ts <repo-root>\n');
    process.exitCode = 2;
  } else {
    const read = (file: string): string => readFileSync(path.resolve(root, file), 'utf8');
    const result = check(
      read(DENY),
      read(CONFIG),
      read(NPM_LOCK),
      read(CARGO_LOCK),
      listCrates(path.resolve(root)),
    );
    process.stdout.write(result.out);
    process.exitCode = result.code;
  }
}
