// Deletes the main branch's caches that a newer cache of the same family has
// superseded. Each family keeps only its newest cache (by creation time); a
// superseded set otherwise stays until GitHub's idle eviction and pushes the
// repository towards its cache size limit.
//
//   node crates/launcher/scripts/cache-prune.ts            print the plan
//   node crates/launcher/scripts/cache-prune.ts --delete   print it, then delete
//
// Family: the key without its trailing hashes or version fields. A key in no
// family (CodeQL's own caches, anything new) is kept untouched. Fails on a list
// that may be truncated, on an entry it cannot read, and on any failed delete.
// Exit status: 0 pass, 1 fail, 2 usage.
import { execFileSync } from 'node:child_process';
import path from 'node:path';

export const MAIN_REF = 'refs/heads/main';
export const LIST_LIMIT = 1000;

// rust-cache: v0-rust-<job>-<os>-<arch>-<environment hash>-<lockfile hash>
// setup-node: node-cache-<os>-<arch>-npm-<lockfile hash>
// the Windows SDK cache: xwin-sdk-<image>-<cargo-xwin>-<sdk>-<crt>
const FAMILIES: readonly RegExp[] = [
  /^(v0-rust-[a-z0-9-]+-(?:Linux|Windows_NT|macOS)-[A-Za-z0-9]+)-[0-9a-f]{8}-[0-9a-f]{8}$/u,
  /^(node-cache-[A-Za-z]+-[A-Za-z0-9]+-npm)-[0-9a-f]{64}$/u,
  /^(xwin-sdk-[a-z0-9]+)-[^-]+-[^-]+-[^-]+$/u,
];

export interface Cache {
  readonly id: number;
  readonly key: string;
  readonly sizeInBytes: number;
  readonly createdAt: string;
}

export const familyOf = (key: string): string | null => {
  for (const pattern of FAMILIES) {
    const family = pattern.exec(key)?.[1];
    if (family !== undefined) return family;
  }
  return null;
};

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const gib = (bytes: number): string => `${(bytes / 2 ** 30).toFixed(2)} GiB`;
const mib = (bytes: number): string => `${(bytes / 2 ** 20).toFixed(1)} MiB`;
const sum = (caches: readonly Cache[]): number =>
  caches.reduce((total, cache) => total + cache.sizeInBytes, 0);

const parse = (listJson: string): { caches: Cache[]; problems: string[] } => {
  let data: unknown;
  try {
    data = JSON.parse(listJson);
  } catch {
    return { caches: [], problems: ['the cache list is not JSON'] };
  }
  if (!Array.isArray(data)) return { caches: [], problems: ['the cache list is not a JSON array'] };
  const caches: Cache[] = [];
  const problems: string[] = [];
  data.forEach((entry: unknown, i: number) => {
    const ok =
      isRecord(entry) &&
      Number.isSafeInteger(entry['id']) &&
      typeof entry['key'] === 'string' &&
      Number.isSafeInteger(entry['sizeInBytes']) &&
      typeof entry['createdAt'] === 'string' &&
      !Number.isNaN(Date.parse(entry['createdAt']));
    if (!ok) {
      problems.push(`entry ${i} lacks a numeric id and size, a key or a creation time`);
      return;
    }
    if (entry['ref'] !== MAIN_REF) {
      problems.push(`entry ${i} (${String(entry['key'])}) is not on ${MAIN_REF}`);
      return;
    }
    caches.push({
      id: Number(entry['id']),
      key: String(entry['key']),
      sizeInBytes: Number(entry['sizeInBytes']),
      createdAt: String(entry['createdAt']),
    });
  });
  return { caches, problems };
};

const newestFirst = (a: Cache, b: Cache): number => {
  const byTime = Date.parse(b.createdAt) - Date.parse(a.createdAt);
  return byTime === 0 ? b.id - a.id : byTime;
};

// The first line of a failed gh call's error output.
const reasonOf = (error: unknown): string => {
  const stderr = isRecord(error) ? error['stderr'] : undefined;
  const text = typeof stderr === 'string' ? stderr.trim() : '';
  const fallback = error instanceof Error ? error.message : 'unknown error';
  return (text === '' ? fallback : text).split('\n')[0] ?? '';
};

// The ids to delete: every cache of a family but its newest.
export const plan = (caches: readonly Cache[]): { keep: Cache[]; remove: Cache[] } => {
  const families = new Map<string, Cache[]>();
  const keep: Cache[] = [];
  for (const cache of caches) {
    const family = familyOf(cache.key);
    if (family === null) keep.push(cache);
    else families.set(family, [...(families.get(family) ?? []), cache]);
  }
  const remove: Cache[] = [];
  for (const members of families.values()) {
    const [newest, ...older] = [...members].sort(newestFirst);
    if (newest !== undefined) keep.push(newest);
    remove.push(...older);
  }
  return { keep, remove };
};

export const run = (
  listJson: string,
  remove: ((id: number) => void) | null,
  limit = LIST_LIMIT,
): { code: number; out: string } => {
  const lines: string[] = [];
  const say = (line: string): void => {
    lines.push(`cache prune: ${line}`);
  };
  const { caches, problems } = parse(listJson);
  for (const problem of problems) say(`FAIL ${problem}`);
  const truncated = caches.length + problems.length >= limit;
  if (truncated) say(`FAIL the list reached its limit of ${limit} entries and may be truncated`);
  const result = plan(caches);
  const rows = [
    ...result.keep.map((cache) => ({ cache, verdict: 'keep' })),
    ...result.remove.map((cache) => ({ cache, verdict: 'delete' })),
  ].sort((a, b) => {
    const byFamily = (familyOf(a.cache.key) ?? '').localeCompare(familyOf(b.cache.key) ?? '');
    return byFamily === 0 ? newestFirst(a.cache, b.cache) : byFamily;
  });
  for (const { cache, verdict } of rows) {
    const family = familyOf(cache.key) ?? 'no family';
    say(`${verdict} ${cache.key} (${mib(cache.sizeInBytes)}, ${cache.createdAt}) [${family}]`);
  }
  say(
    `listed ${caches.length} caches (${gib(sum(caches))}); keep ${result.keep.length} (${gib(sum(result.keep))}), delete ${result.remove.length} (${gib(sum(result.remove))})`,
  );
  if (remove !== null && problems.length === 0 && !truncated) {
    let deleted = 0;
    for (const cache of result.remove) {
      try {
        remove(cache.id);
        deleted += 1;
      } catch (error: unknown) {
        say(`FAIL delete ${cache.id} ${cache.key}: ${reasonOf(error)}`);
      }
    }
    say(`deleted ${deleted} of ${result.remove.length}`);
  } else if (remove !== null) {
    say('deleted nothing: the list was not read whole');
  }
  const failed = lines.some((line) => line.startsWith('cache prune: FAIL '));
  return { code: failed ? 1 : 0, out: `${lines.join('\n')}\n` };
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  const args = process.argv.slice(2);
  if (args.length > 1 || (args.length === 1 && args[0] !== '--delete')) {
    process.stderr.write('usage: cache-prune.ts [--delete]\n');
    process.exitCode = 2;
  } else {
    const repo = process.env['GITHUB_REPOSITORY'] ?? '';
    const repoArgs = repo === '' ? [] : ['--repo', repo];
    const fields = 'id,key,ref,sizeInBytes,createdAt';
    let listJson = '';
    try {
      listJson = execFileSync(
        'gh',
        [
          'cache',
          'list',
          ...repoArgs,
          '--ref',
          MAIN_REF,
          '--limit',
          String(LIST_LIMIT),
          '--json',
          fields,
        ],
        { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
      );
    } catch (error: unknown) {
      process.stdout.write(`cache prune: FAIL gh cache list: ${reasonOf(error)}\n`);
    }
    const remove =
      args[0] === '--delete'
        ? (id: number): void => {
            execFileSync('gh', ['cache', 'delete', String(id), ...repoArgs], {
              encoding: 'utf8',
              stdio: ['ignore', 'ignore', 'pipe'],
            });
          }
        : null;
    const result = run(listJson, remove);
    process.stdout.write(result.out);
    process.exitCode = result.code;
  }
}
