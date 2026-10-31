// Self-test of the cache prune plan: the main branch's real cache list of one
// day, plus planted entries, each with the verdict it must get.
//
//   node crates/launcher/scripts/cache-prune.selftest.ts
import { MAIN_REF, run } from './cache-prune.ts';

// id, key, size in bytes, creation time: `gh cache list --ref refs/heads/main`
// as measured on 2026-11-01.
const MEASURED: readonly (readonly [number, string, number, string])[] = [
  [
    9798344824,
    'node-cache-Windows-x64-npm-5d213a09bdcfe196981675b03f1c30f942bf86f29304033f5905d518926cd0d0',
    46073943,
    '2026-10-31T12:11:32.111731Z',
  ],
  [
    9798255088,
    'node-cache-Linux-x64-npm-5d213a09bdcfe196981675b03f1c30f942bf86f29304033f5905d518926cd0d0',
    48523270,
    '2026-10-31T12:07:22.104542Z',
  ],
  [
    9807654919,
    'codeql-overlay-base-database-1-c801913f1ee29663-javascript-2.27.3-ac0dc21fb45fb5bc2de05fe32ef67b9f349d40f3-41232423668-1',
    23450853,
    '2026-10-31T19:04:59.371934Z',
  ],
  [
    9798355430,
    'v0-rust-coverage-Windows_NT-x64-68a5225a-b91c19e1',
    610498778,
    '2026-10-31T12:12:07.737649Z',
  ],
  [
    9798439107,
    'v0-rust-rust-Windows_NT-x64-68a5225a-b91c19e1',
    780552578,
    '2026-10-31T12:15:39.45719Z',
  ],
  [
    8326064270,
    'xwin-sdk-ubuntu24-0.23.1-10.0.26100-14.44.17.14',
    137637658,
    '2026-09-30T17:38:25.763066Z',
  ],
  [
    9798299723,
    'v0-rust-release-audit-Linux-x64-fcd0b652-b91c19e1',
    641644937,
    '2026-10-31T12:09:29.388953Z',
  ],
  [
    9798368246,
    'v0-rust-rust-host-Linux-x64-fcd0b652-b91c19e1',
    1047379634,
    '2026-10-31T12:12:33.227103Z',
  ],
  [
    9798267990,
    'codeql-overlay-base-database-1-c801913f1ee29663-javascript-2.27.3-a8f3bd670afe68efcdd0de968ee5ccdc262e6aee-41199653160-1',
    23563104,
    '2026-10-31T12:07:59.993698Z',
  ],
  [
    9389774581,
    'v0-rust-coverage-Windows_NT-x64-68a5225a-feb8e9d4',
    621252158,
    '2026-10-23T08:48:48.268923Z',
  ],
  [
    9353322366,
    'v0-rust-rust-Windows_NT-x64-68a5225a-feb8e9d4',
    796202129,
    '2026-10-22T16:41:46.5263Z',
  ],
  [
    9353086499,
    'v0-rust-rust-host-Linux-x64-fcd0b652-feb8e9d4',
    1049469458,
    '2026-10-22T16:36:35.726957Z',
  ],
  [
    9693841948,
    'v0-rust-release-audit-Linux-x64-fcd0b652-feb8e9d4',
    539404540,
    '2026-10-29T10:19:01.428097Z',
  ],
  [
    9576325536,
    'codeql-overlay-base-database-1-c801913f1ee29663-javascript-2.27.3-172edcfa2422682d7fa4172168361f02da47c105-40490837103-1',
    23479955,
    '2026-10-27T10:45:49.796829Z',
  ],
  [
    9353178603,
    'node-cache-Windows-x64-npm-d3cbe7088a06e2b48c9be2d9b14b521ea74415453563b8d2596367ee14a286b3',
    45654812,
    '2026-10-22T16:38:37.133446Z',
  ],
  [
    9352939183,
    'node-cache-Linux-x64-npm-d3cbe7088a06e2b48c9be2d9b14b521ea74415453563b8d2596367ee14a286b3',
    47731072,
    '2026-10-22T16:33:22.150005Z',
  ],
  [
    9390627840,
    'v0-rust-rust-host-Linux-x64-34fde5c2-feb8e9d4',
    1953130811,
    '2026-10-23T09:08:09.43764Z',
  ],
  [
    9353175447,
    'v0-rust-release-audit-Linux-x64-34fde5c2-feb8e9d4',
    952780918,
    '2026-10-22T16:38:34.469305Z',
  ],
];
// The superseded sets: older lockfile and environment hashes of each family.
const SUPERSEDED = [
  9389774581, 9353322366, 9353086499, 9693841948, 9353178603, 9352939183, 9390627840, 9353175447,
];

type Entry = Readonly<Record<string, unknown>>;
const entry = ([id, key, sizeInBytes, createdAt]: readonly [
  number,
  string,
  number,
  string,
]): Entry => ({
  id,
  key,
  ref: MAIN_REF,
  sizeInBytes,
  createdAt,
  lastAccessedAt: createdAt,
});
const REAL: readonly Entry[] = MEASURED.map(entry);
const UNKNOWN = entry([1, 'some-other-tool-cache-0123abcd', 1024, '2026-09-01T00:00:00Z']);
const UNKNOWN_NEWER = entry([2, 'some-other-tool-cache-4567ef01', 1024, '2026-10-01T00:00:00Z']);

interface Case {
  readonly name: string;
  readonly list: string;
  readonly expect: number;
  // The ids the run must try to delete, in any order (null: no delete run).
  readonly deletes: readonly number[] | null;
  // A line the run must print.
  readonly says: string;
  // An id whose delete fails.
  readonly failing?: number;
  readonly limit?: number;
}

const list = (entries: readonly Entry[]): string => JSON.stringify(entries);

const cases: readonly Case[] = [
  {
    name: 'the measured list',
    list: list([...REAL, UNKNOWN, UNKNOWN_NEWER]),
    expect: 0,
    deletes: SUPERSEDED,
    says: 'listed 20 caches (8.74 GiB); keep 12 (3.15 GiB), delete 8 (5.59 GiB)',
  },
  {
    name: 'the measured list in reverse order',
    list: list([...REAL].reverse()),
    expect: 0,
    deletes: SUPERSEDED,
    says: 'deleted 8 of 8',
  },
  {
    name: 'a key in no family is kept',
    list: list([...REAL, UNKNOWN]),
    expect: 0,
    deletes: SUPERSEDED,
    says: 'keep some-other-tool-cache-0123abcd',
  },
  {
    name: 'a CodeQL cache is kept',
    list: list(REAL),
    expect: 0,
    deletes: SUPERSEDED,
    says: 'keep codeql-overlay-base-database-1-c801913f1ee29663-javascript-2.27.3-172edcfa',
  },
  {
    name: 'a newer environment hash supersedes the older one',
    list: list(REAL),
    expect: 0,
    deletes: SUPERSEDED,
    says: 'delete v0-rust-rust-host-Linux-x64-34fde5c2-feb8e9d4',
  },
  {
    name: 'one cache per family deletes nothing',
    list: list(REAL.filter((item) => !SUPERSEDED.includes(Number(item['id'])))),
    expect: 0,
    deletes: [],
    says: 'delete 0 (0.00 GiB)',
  },
  {
    name: 'a failed delete',
    list: list(REAL),
    expect: 1,
    deletes: SUPERSEDED,
    failing: 9390627840,
    says: 'FAIL delete 9390627840 v0-rust-rust-host-Linux-x64-34fde5c2-feb8e9d4: HTTP 404',
  },
  {
    name: 'a list at its limit',
    list: list(REAL),
    expect: 1,
    deletes: [],
    limit: MEASURED.length,
    says: 'may be truncated',
  },
  {
    name: 'an entry of another branch',
    list: list([...REAL, { ...UNKNOWN, ref: 'refs/pull/1/merge' }]),
    expect: 1,
    deletes: [],
    says: 'is not on refs/heads/main',
  },
  {
    name: 'an entry without a size',
    list: list([...REAL, { ...UNKNOWN, sizeInBytes: '1024' }]),
    expect: 1,
    deletes: [],
    says: 'entry 18 lacks',
  },
  { name: 'no JSON', list: '', expect: 1, deletes: [], says: 'FAIL the cache list is not JSON' },
  { name: 'not an array', list: '{}', expect: 1, deletes: [], says: 'not a JSON array' },
];

const failures: string[] = [];
for (const c of cases) {
  const tried: number[] = [];
  const remove = (id: number): void => {
    tried.push(id);
    if (id === c.failing) throw Object.assign(new Error('gh failed'), { stderr: 'HTTP 404\n' });
  };
  const result = run(c.list, remove, c.limit);
  const problems: string[] = [];
  if (result.code !== c.expect) problems.push(`exit ${result.code}, expected ${c.expect}`);
  if (!result.out.includes(c.says)) problems.push(`no output holding "${c.says}"`);
  const want = [...(c.deletes ?? [])].sort((a, b) => a - b).join(',');
  const got = [...tried].sort((a, b) => a - b).join(',');
  if (got !== want) problems.push(`deleted [${got}], expected [${want}]`);
  if (problems.length > 0) failures.push(`${c.name}: ${problems.join('; ')}`);
}
for (const failure of failures)
  process.stdout.write(`cache prune self-test: not as expected: ${failure}\n`);
const expected = cases.length - failures.length;
process.stdout.write(`cache prune self-test: cases ${cases.length}, as expected ${expected}\n`);
process.exitCode = cases.length > 0 && expected === cases.length ? 0 : 1;
