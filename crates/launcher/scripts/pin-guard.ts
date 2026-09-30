// Checks that every version a gate depends on is read from its pin file
// (rust-toolchain.toml, .nvmrc, scripts/ci-tools.env) and stated nowhere else
// in the workflows and the two gate scripts, that the pin files keep their
// shape, and that .github/dependabot.yml keeps its cooldown, groups and
// reasoned ignores. Each rule prints what it scanned; any violation fails, and
// so does a rule that scanned nothing.
//
//   node crates/launcher/scripts/pin-guard.ts
//
// It reads the files itself and the step table's tools through
// `bash scripts/ci-check.sh --list`.
import { execFileSync } from 'node:child_process';
import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { TOOL_SOURCES } from './freshness.ts';
import type { ToolSource } from './freshness.ts';

export type Rule =
  | 'toolchain names'
  | 'Node version source'
  | 'runner labels'
  | 'action pins'
  | 'install-action entries'
  | 'version literals'
  | '@types/node major'
  | 'pin file shapes'
  | 'Dependabot policy';

export interface Violation {
  readonly rule: Rule;
  readonly file: string;
  readonly line: number | null;
  readonly text: string;
}

export interface Scan {
  readonly rule: Rule;
  readonly scanned: number;
  readonly unit: string;
}

export interface PinInput {
  // Path (from the repository root) to text, for every file that was read.
  readonly files: ReadonlyMap<string, string>;
  // Scanned files that exist but could not be read, with the error code.
  readonly unreadable?: ReadonlyMap<string, string>;
  readonly toolSources: readonly ToolSource[];
  // The tools of every step-table row; null when the table could not be listed.
  readonly runnerTools: readonly string[] | null;
}

export interface PinResult {
  readonly violations: readonly Violation[];
  readonly scans: readonly Scan[];
}

const SCRIPTS = ['scripts/ci-check.sh', 'scripts/build.sh'];
const TOOLCHAIN_FILE = 'rust-toolchain.toml';
const NVMRC = '.nvmrc';
const TOOL_FILE = 'scripts/ci-tools.env';
const PACKAGE = 'crates/launcher/package.json';
const DEPENDABOT = '.github/dependabot.yml';
export const PIN_GUARD_FILES: readonly string[] = [
  ...SCRIPTS,
  TOOLCHAIN_FILE,
  NVMRC,
  TOOL_FILE,
  PACKAGE,
  DEPENDABOT,
];
const RUNNER_LABELS = ['ubuntu-24.04', 'windows-2025-vs2026'];
const TOOLCHAIN_KEYS = ['channel', 'components', 'targets', 'profile'];
const EXPORTED_KEYS = ['XWIN_SDK_VERSION', 'XWIN_CRT_VERSION'];
// One beta override per canary job.
const BETA_OVERRIDES = 2;

interface Source {
  readonly file: string;
  readonly lines: readonly string[];
}

const sourcesOf = (
  files: ReadonlyMap<string, string>,
  wanted: (file: string) => boolean,
): Source[] =>
  [...files.entries()]
    .filter(([file]) => wanted(file))
    .map(([file, text]) => ({ file, lines: text.split('\n') }));

const isWorkflow = (file: string): boolean => /^\.github\/workflows\/[^/]+\.ya?ml$/u.test(file);

const lineCount = (sources: readonly Source[]): number =>
  sources.reduce((sum, source) => sum + source.lines.length, 0);

const indentOf = (line: string): number => line.length - line.trimStart().length;

const ITEM = /^\s*-\s/u;

// The lines of the sequence item (a workflow step) that holds line `at`.
const stepBlock = (lines: readonly string[], at: number): readonly string[] => {
  let start = at;
  const own = indentOf(lines[at] ?? '');
  const above = ITEM.test(lines[at] ?? '') ? -1 : at - 1;
  for (let j = above; j >= 0; j -= 1) {
    const text = lines[j] ?? '';
    if (text.trim() !== '' && indentOf(text) < own) {
      start = ITEM.test(text) ? j : at;
      break;
    }
  }
  const dash = indentOf(lines[start] ?? '');
  let end = start + 1;
  while (
    end < lines.length &&
    ((lines[end] ?? '').trim() === '' || indentOf(lines[end] ?? '') > dash)
  )
    end += 1;
  return lines.slice(start, end);
};

// A key's value on a YAML line (after an optional sequence dash), without a
// trailing comment; null when the line holds another key.
const keyValue = (line: string, key: string): string | null => {
  const found = new RegExp(`^\\s*(?:-\\s+)?${key}:(.*)$`, 'u').exec(line);
  return found === null ? null : (found[1] ?? '').replace(/\s+#.*$/u, '').trim();
};

type Add = (rule: Rule, file: string, line: number | null, text: string) => void;

// Options of `rustup toolchain install` that take a value.
const OPTION_VALUES = new Set(['--profile', '-c', '--component', '-t', '--target']);

// The first argument that is not an option or an option's value; null when
// the command names no toolchain (the form that installs the file's).
const installArgument = (rest: string): string | null => {
  const words = rest.trim().split(/\s+/u);
  for (let i = 0; i < words.length; i += 1) {
    const word = words[i] ?? '';
    if (word.startsWith('-')) {
      if (OPTION_VALUES.has(word)) i += 1;
    } else {
      return /^[\w.@+/-]+$/u.test(word) ? word : null;
    }
  }
  return null;
};

const toolchainLine = (source: Source, line: string, no: number, add: Add): void => {
  const rule: Rule = 'toolchain names';
  const text = line.trim();
  if (line.includes('dtolnay/rust-toolchain')) add(rule, source.file, no, text);
  if (/(?:^|[^\w-])cargo \+\S/u.test(line)) add(rule, source.file, no, text);
  if (/\brustup\s+(?:default|override)\b/u.test(line)) add(rule, source.file, no, text);
  const install = /\brustup\s+toolchain\s+install\b(.*)$/u.exec(line);
  const argument = install === null ? null : installArgument(install[1] ?? '');
  if (argument !== null && argument !== 'beta') add(rule, source.file, no, text);
};

// Calls visit for every line of every source, with its 1-based number.
const eachLine = (
  sources: readonly Source[],
  visit: (source: Source, line: string, no: number) => void,
): void => {
  for (const source of sources) {
    source.lines.forEach((line, index) => {
      visit(source, line, index + 1);
    });
  }
};

const checkToolchainNames = (
  workflows: readonly Source[],
  scripts: readonly Source[],
  add: Add,
): Scan => {
  const rule: Rule = 'toolchain names';
  let betas = 0;
  eachLine([...workflows, ...scripts], (source, line, no) => {
    toolchainLine(source, line, no, add);
    if (!line.includes('RUSTUP_TOOLCHAIN')) return;
    if (workflows.includes(source)) {
      if (line.trim() === 'RUSTUP_TOOLCHAIN: beta') betas += 1;
      else add(rule, source.file, no, line.trim());
    } else if (
      /(?:^|[\s;&|])RUSTUP_TOOLCHAIN=/u.test(line) ||
      /\bexport\s+RUSTUP_TOOLCHAIN\b/u.test(line)
    ) {
      add(rule, source.file, no, line.trim());
    }
  });
  if (betas !== BETA_OVERRIDES) {
    add(
      rule,
      '.github/workflows',
      null,
      `RUSTUP_TOOLCHAIN: beta appears ${String(betas)} times in the workflows, expected ${String(BETA_OVERRIDES)} (one per beta canary job)`,
    );
  }
  return { rule, scanned: lineCount([...workflows, ...scripts]), unit: 'lines' };
};

const checkNodeSource = (workflows: readonly Source[], add: Add): Scan => {
  const rule: Rule = 'Node version source';
  eachLine(workflows, (source, line, no) => {
    if (keyValue(line, 'node-version') !== null) add(rule, source.file, no, line.trim());
    if (!(keyValue(line, 'uses') ?? '').startsWith('actions/setup-node@')) return;
    const block = stepBlock(source.lines, no - 1);
    if (!block.some((text) => text.trim() === 'node-version-file: .nvmrc'))
      add(rule, source.file, no, 'a setup-node step without node-version-file: .nvmrc');
  });
  return { rule, scanned: lineCount(workflows), unit: 'workflow lines' };
};

const checkRunnerLabels = (workflows: readonly Source[], add: Add): Scan => {
  const rule: Rule = 'runner labels';
  let scanned = 0;
  eachLine(workflows, (source, line, no) => {
    const value = keyValue(line, 'runs-on');
    if (value === null) return;
    scanned += 1;
    if (!RUNNER_LABELS.includes(value)) add(rule, source.file, no, line.trim());
  });
  return { rule, scanned, unit: 'runs-on lines' };
};

const ACTION_PIN =
  /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_./-]+)?@[0-9a-f]{40} # v\d+\.\d+\.\d+$/u;

const checkActionPins = (workflows: readonly Source[], add: Add): Scan => {
  const rule: Rule = 'action pins';
  let scanned = 0;
  eachLine(workflows, (source, line, no) => {
    const found = /^\s*(?:-\s+)?uses:\s*(.*)$/u.exec(line);
    if (found === null) return;
    scanned += 1;
    if (!ACTION_PIN.test((found[1] ?? '').trim())) add(rule, source.file, no, line.trim());
  });
  return { rule, scanned, unit: 'uses lines' };
};

const TOOL_ENTRY = /^[a-z0-9][\w.-]*@\$\{\{ steps\.[\w-]+\.outputs\.[A-Z][A-Z0-9_]* \}\}$/u;

const checkInstallAction = (workflows: readonly Source[], add: Add): Scan => {
  const rule: Rule = 'install-action entries';
  eachLine(workflows, (source, line, no) => {
    const tools = keyValue(line, 'tool');
    for (const entry of tools === null ? [] : tools.split(',')) {
      if (!TOOL_ENTRY.test(entry.trim())) add(rule, source.file, no, `tool: ${entry.trim()}`);
    }
    if (!(keyValue(line, 'uses') ?? '').startsWith('taiki-e/install-action@')) return;
    if (!stepBlock(source.lines, no - 1).some((text) => text.trim() === 'fallback: none'))
      add(rule, source.file, no, 'an install-action step without fallback: none');
  });
  return { rule, scanned: lineCount(workflows), unit: 'workflow lines' };
};

const VERSION_LITERAL = /(?<![\w.])v?\d+\.\d+\.\d+(?![\w]|\.\d)/gu;
const HEX_LITERAL = /(?<![0-9A-Fa-f])[0-9a-f]{64}(?![0-9A-Fa-f])/gu;

const checkVersionLiterals = (sources: readonly Source[], add: Add): Scan => {
  const rule: Rule = 'version literals';
  eachLine(sources, (source, line, no) => {
    // A uses: line's exact-version comment is the one place a version is written.
    const text = line.replace(/^(\s*(?:-\s+)?uses:.*?) # v\d+\.\d+\.\d+$/u, '$1');
    for (const found of [...text.matchAll(VERSION_LITERAL), ...text.matchAll(HEX_LITERAL)])
      add(rule, source.file, no, found[0]);
  });
  return { rule, scanned: lineCount(sources), unit: 'lines' };
};

const checkTypesMajor = (files: ReadonlyMap<string, string>, add: Add): Scan => {
  const rule: Rule = '@types/node major';
  const lines = (files.get(PACKAGE) ?? '').split('\n');
  const index = lines.findIndex((line) => /"@types\/node"\s*:/u.test(line));
  if (index < 0) return { rule, scanned: 0, unit: 'ranges' };
  const range = /"@types\/node"\s*:\s*"([^"]*)"/u.exec(lines[index] ?? '')?.[1] ?? '';
  const major = /\d+/u.exec(range)?.[0];
  const node = /\d+/u.exec(files.get(NVMRC) ?? '')?.[0];
  if (node !== undefined && major !== node)
    add(
      rule,
      PACKAGE,
      index + 1,
      `"@types/node": "${range}" (major ${major ?? 'none'}, .nvmrc major ${node})`,
    );
  return { rule, scanned: 1, unit: 'ranges' };
};

const checkToolchainFile = (text: string, add: Add): void => {
  const rule: Rule = 'pin file shapes';
  const seen = new Set<string>();
  let tables = 0;
  text.split('\n').forEach((line, index) => {
    const trimmed = line.trim();
    if (trimmed === '' || trimmed.startsWith('#')) return;
    if (trimmed === '[toolchain]') {
      tables += 1;
      return;
    }
    const found = /^([a-z-]+)\s*=\s*(.+)$/u.exec(trimmed);
    const key = found?.[1] ?? '';
    if (found === null || tables !== 1 || !TOOLCHAIN_KEYS.includes(key) || seen.has(key)) {
      add(
        rule,
        TOOLCHAIN_FILE,
        index + 1,
        `${trimmed} is not one of the keys ${TOOLCHAIN_KEYS.join(', ')} under [toolchain], once each`,
      );
      return;
    }
    seen.add(key);
    if (key === 'channel' && !/^"\d+\.\d+\.\d+"$/u.test(found[2] ?? ''))
      add(rule, TOOLCHAIN_FILE, index + 1, `${trimmed} is not an exact X.Y.Z version`);
  });
  if (tables !== 1)
    add(rule, TOOLCHAIN_FILE, null, `holds ${String(tables)} [toolchain] tables, not 1`);
  for (const key of TOOLCHAIN_KEYS.filter((wanted) => !seen.has(wanted)))
    add(rule, TOOLCHAIN_FILE, null, `lacks the key ${key}`);
};

const checkNvmrc = (text: string, add: Add): void => {
  if (!/^\d+\.\d+\.\d+\n?$/u.test(text))
    add('pin file shapes', NVMRC, 1, `${JSON.stringify(text)} is not one X.Y.Z line`);
};

// The tool file's lines and keys, and its key set against the freshness
// report's tool list both ways. Returns the number of keys.
const checkToolFile = (text: string, sources: readonly ToolSource[], add: Add): number => {
  const rule: Rule = 'pin file shapes';
  const keys = new Map<string, number>();
  text.split('\n').forEach((line, index) => {
    if (line === '' || line.startsWith('#')) return;
    const found = /^([A-Z][A-Z0-9_]*)=(\S+)$/u.exec(line);
    if (found === null) {
      add(rule, TOOL_FILE, index + 1, `${line} is not KEY=VALUE, a # comment or blank`);
      return;
    }
    const key = found[1] ?? '';
    if (keys.has(key)) add(rule, TOOL_FILE, index + 1, `the key ${key} is repeated`);
    keys.set(key, index + 1);
    if (key.endsWith('_SHA256') && !/^[0-9a-f]{64}$/u.test(found[2] ?? ''))
      add(rule, TOOL_FILE, index + 1, `${key} is not 64 lowercase hex digits`);
  });
  const expected = [
    ...sources.map((source) => source.key),
    ...sources.filter((source) => source.manifest === null).map((source) => `${source.key}_SHA256`),
    ...EXPORTED_KEYS,
  ];
  for (const key of expected.filter((wanted) => !keys.has(wanted)))
    add(rule, TOOL_FILE, null, `lacks the key ${key}`);
  for (const [key, no] of [...keys.entries()].filter(([found]) => !expected.includes(found)))
    add(rule, TOOL_FILE, no, `the key ${key} is not in the freshness report's tool list`);
  return keys.size;
};

const toolKey = (tool: string): string => tool.toUpperCase().replaceAll('-', '_');

const checkRunnerTools = (input: PinInput, add: Add): void => {
  if (input.runnerTools === null) {
    add('pin file shapes', 'scripts/ci-check.sh', null, 'runner table unreadable');
    return;
  }
  const fileKeys = new Set(
    [...(input.files.get(TOOL_FILE) ?? '').matchAll(/^([A-Z][A-Z0-9_]*)=/gmu)].map(
      (found) => found[1],
    ),
  );
  for (const tool of new Set(input.runnerTools)) {
    const key = toolKey(tool);
    if (input.toolSources.some((source) => source.key === key) && !fileKeys.has(key))
      add(
        'pin file shapes',
        TOOL_FILE,
        null,
        `the runner tool ${tool} names ${key}, which the tool file lacks`,
      );
  }
};

// The pin files' presence and shape. Returns the number of tool-file keys.
const checkPinFiles = (input: PinInput, add: Add): Scan => {
  const rule: Rule = 'pin file shapes';
  for (const [file, code] of input.unreadable ?? new Map<string, string>())
    add(rule, file, null, `unreadable (${code})`);
  for (const file of PIN_GUARD_FILES.filter((wanted) => !input.files.has(wanted))) {
    if (!(input.unreadable?.has(file) ?? false)) add(rule, file, null, 'missing');
  }
  const toolchain = input.files.get(TOOLCHAIN_FILE);
  if (toolchain !== undefined) checkToolchainFile(toolchain, add);
  const nvmrc = input.files.get(NVMRC);
  if (nvmrc !== undefined) checkNvmrc(nvmrc, add);
  const tools = input.files.get(TOOL_FILE);
  const keys = tools === undefined ? 0 : checkToolFile(tools, input.toolSources, add);
  checkRunnerTools(input, add);
  return { rule, scanned: keys, unit: 'tool-file keys' };
};

const ECOSYSTEMS = ['cargo', 'npm', 'github-actions', 'rust-toolchain'];
// The ecosystems with more than one dependency, whose minor and patch updates
// arrive as one grouped pull request.
const GROUPED = ['cargo', 'npm', 'github-actions'];
const COOLDOWN_DAYS = '7';

interface Group {
  types: readonly string[];
}

interface Ignore {
  readonly name: string;
  readonly line: number;
  readonly commented: boolean;
}

interface Ecosystem {
  readonly name: string;
  readonly line: number;
  days: { readonly value: string; readonly line: number } | null;
  readonly groups: Group[];
  readonly ignores: Ignore[];
}

type Section = '' | 'schedule' | 'cooldown' | 'groups' | 'group' | 'ignore' | 'ignored';

interface Parse {
  readonly ecosystems: Ecosystem[];
  section: Section;
}

const SECTIONS: Readonly<Record<string, Section>> = {
  directory: '',
  schedule: 'schedule',
  cooldown: 'cooldown',
  groups: 'groups',
  ignore: 'ignore',
};

const unquote = (value: string): string => value.trim().replace(/^(['"])(.*)\1$/u, '$2');

const flowList = (value: string): readonly string[] =>
  (/^\[(.*)\]$/u.exec(value.trim())?.[1] ?? '')
    .split(',')
    .map(unquote)
    .filter((item) => item !== '');

// A line under a section key (indent 6): an interval, the cooldown, a group
// name or an ignored dependency.
const sectionLine = (
  state: Parse,
  eco: Ecosystem,
  text: string,
  no: number,
  commented: boolean,
): boolean => {
  if (state.section === 'schedule') return /^interval: \S+$/u.test(text);
  const days = /^default-days: (\S+)$/u.exec(text);
  if (state.section === 'cooldown' && days !== null) {
    eco.days = { value: days[1] ?? '', line: no };
    return true;
  }
  if ((state.section === 'groups' || state.section === 'group') && /^[a-z0-9-]+:$/u.test(text)) {
    eco.groups.push({ types: [] });
    state.section = 'group';
    return true;
  }
  const ignored = /^- dependency-name: (.+)$/u.exec(text);
  if ((state.section === 'ignore' || state.section === 'ignored') && ignored !== null) {
    eco.ignores.push({ name: unquote(ignored[1] ?? ''), line: no, commented });
    state.section = 'ignored';
    return true;
  }
  return false;
};

// A line under a group or an ignored dependency (indent 8).
const itemLine = (state: Parse, eco: Ecosystem, text: string): boolean => {
  const types = /^update-types: (\[.*\])$/u.exec(text);
  const group = eco.groups.at(-1);
  if (state.section === 'group' && group !== undefined) {
    if (types !== null) group.types = flowList(types[1] ?? '');
    return types !== null || /^patterns: \[.*\]$/u.test(text);
  }
  return state.section === 'ignored' && types !== null;
};

// One line of the constrained shape dependabot.yml is written in; false for
// any line that shape does not hold.
const dependabotLine = (state: Parse, line: string, no: number, commented: boolean): boolean => {
  const indent = indentOf(line);
  const text = line.trim();
  const start = /^- package-ecosystem: (.+)$/u.exec(text);
  if (indent === 0) return text === 'version: 2' || text === 'updates:';
  if (indent === 2 && start !== null) {
    const name = unquote(start[1] ?? '');
    state.ecosystems.push({ name, line: no, days: null, groups: [], ignores: [] });
    state.section = '';
    return true;
  }
  const eco = state.ecosystems.at(-1);
  if (eco === undefined) return false;
  if (indent === 4) {
    const key = /^([a-z]+):(.*)$/u.exec(text);
    const section = SECTIONS[key?.[1] ?? ''];
    if (key === null || section === undefined) return false;
    state.section = section;
    return (key[2] ?? '').trim() === '' ? key[1] !== 'directory' : key[1] === 'directory';
  }
  if (indent === 6) return sectionLine(state, eco, text, no, commented);
  return indent === 8 && itemLine(state, eco, text);
};

const checkEcosystem = (eco: Ecosystem, add: Add): void => {
  const rule: Rule = 'Dependabot policy';
  if (eco.days === null) add(rule, DEPENDABOT, eco.line, `${eco.name} has no cooldown`);
  else if (eco.days.value !== COOLDOWN_DAYS)
    add(
      rule,
      DEPENDABOT,
      eco.days.line,
      `${eco.name} cooldown default-days is ${eco.days.value}, expected ${COOLDOWN_DAYS}`,
    );
  const minorPatch = eco.groups.some(
    (group) =>
      group.types.length === 2 && group.types.includes('minor') && group.types.includes('patch'),
  );
  if (GROUPED.includes(eco.name) && !minorPatch)
    add(rule, DEPENDABOT, eco.line, `${eco.name} has no group limited to minor and patch updates`);
  for (const ignore of eco.ignores) {
    if (!ignore.commented)
      add(
        rule,
        DEPENDABOT,
        ignore.line,
        `${eco.name} ignores ${ignore.name} with no comment directly above`,
      );
    if (ignore.name === 'windows')
      add(rule, DEPENDABOT, ignore.line, `${eco.name} ignores windows`);
  }
};

const checkDependabot = (text: string | undefined, add: Add): Scan => {
  const rule: Rule = 'Dependabot policy';
  if (text === undefined) return { rule, scanned: 0, unit: 'ecosystems' };
  const state: Parse = { ecosystems: [], section: '' };
  let commentAbove = false;
  text.split('\n').forEach((line, index) => {
    const trimmed = line.trim();
    const commented = commentAbove;
    commentAbove = trimmed.startsWith('#');
    if (trimmed === '' || commentAbove) return;
    if (!dependabotLine(state, line.trimEnd(), index + 1, commented))
      add(rule, DEPENDABOT, index + 1, `an unrecognised line: ${trimmed}`);
  });
  const names = state.ecosystems.map((eco) => eco.name);
  for (const name of ECOSYSTEMS.filter((wanted) => !names.includes(wanted)))
    add(rule, DEPENDABOT, null, `lacks the ${name} ecosystem`);
  state.ecosystems.forEach((eco, index) => {
    if (!ECOSYSTEMS.includes(eco.name) || names.indexOf(eco.name) !== index)
      add(
        rule,
        DEPENDABOT,
        eco.line,
        `${eco.name} is not one of ${ECOSYSTEMS.join(', ')}, once each`,
      );
    checkEcosystem(eco, add);
  });
  return { rule, scanned: state.ecosystems.length, unit: 'ecosystems' };
};

export function checkPins(input: PinInput): PinResult {
  const violations: Violation[] = [];
  const add: Add = (rule, file, line, text) => {
    violations.push({ rule, file, line, text });
  };
  const workflows = sourcesOf(input.files, isWorkflow);
  const scripts = sourcesOf(input.files, (file) => SCRIPTS.includes(file));
  const scans = [
    checkToolchainNames(workflows, scripts, add),
    checkNodeSource(workflows, add),
    checkRunnerLabels(workflows, add),
    checkActionPins(workflows, add),
    checkInstallAction(workflows, add),
    checkVersionLiterals([...workflows, ...scripts], add),
    checkTypesMajor(input.files, add),
    checkPinFiles(input, add),
    checkDependabot(input.files.get(DEPENDABOT), add),
  ];
  return { violations, scans };
}

// Tool-file keys whose tool no step-table row names yet (information only).
export const keysWithoutRunnerRow = (
  sources: readonly ToolSource[],
  runnerTools: readonly string[],
): string[] =>
  sources
    .map((source) => source.key)
    .filter((key) => !runnerTools.some((tool) => toolKey(tool) === key));

const errorCode = (error: unknown): string =>
  error instanceof Error && 'code' in error && typeof error.code === 'string'
    ? error.code
    : 'unknown error';

const readTree = (
  root: string,
): { files: Map<string, string>; unreadable: Map<string, string> } => {
  const files = new Map<string, string>();
  const unreadable = new Map<string, string>();
  const read = (file: string): void => {
    try {
      files.set(file, readFileSync(path.join(root, file), 'utf8'));
    } catch (error: unknown) {
      if (errorCode(error) !== 'ENOENT') unreadable.set(file, errorCode(error));
    }
  };
  try {
    for (const name of readdirSync(path.join(root, '.github', 'workflows')).sort())
      if (/\.ya?ml$/u.test(name)) read(`.github/workflows/${name}`);
  } catch (error: unknown) {
    unreadable.set('.github/workflows', errorCode(error));
  }
  for (const file of PIN_GUARD_FILES) read(file);
  return { files, unreadable };
};

const runnerToolsOf = (root: string): string[] | null => {
  try {
    const rows = execFileSync('bash', ['scripts/ci-check.sh', '--list'], {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    })
      .split('\n')
      .filter((line) => line.startsWith('row\t'));
    if (rows.length === 0) return null;
    return rows
      .flatMap((line) => (line.split('\t')[5] ?? '').split(','))
      .filter((tool) => tool !== '' && tool !== '-');
  } catch {
    return null;
  }
};

const main = (): number => {
  const root = path.resolve(import.meta.dirname, '..', '..', '..');
  const { files, unreadable } = readTree(root);
  const runnerTools = runnerToolsOf(root);
  const result = checkPins({ files, unreadable, toolSources: TOOL_SOURCES, runnerTools });
  const out = (line: string): void => {
    process.stdout.write(`pin guard: ${line}\n`);
  };
  for (const v of result.violations)
    out(
      v.line === null
        ? `${v.file}: ${v.rule}: ${v.text}`
        : `${v.file}:${String(v.line)}: ${v.rule}: ${v.text}`,
    );
  for (const scan of result.scans) {
    const count = result.violations.filter((v) => v.rule === scan.rule).length;
    out(
      scan.scanned === 0
        ? `${scan.rule}: scanned 0 ${scan.unit}, nothing was checked`
        : `${scan.rule}: scanned ${String(scan.scanned)} ${scan.unit}, violations ${String(count)}`,
    );
  }
  const undeclared = keysWithoutRunnerRow(TOOL_SOURCES, runnerTools ?? []);
  out(`tool-file keys no runner row declares yet: ${undeclared.join(', ') || 'none'}`);
  const scanned = (rule: Rule): number =>
    result.scans.find((scan) => scan.rule === rule)?.scanned ?? 0;
  out(
    `${String(files.size)} files, ${String(scanned('action pins'))} uses lines, ${String(scanned('runner labels'))} runs-on lines, ${String(scanned('pin file shapes'))} tool-file keys, ${String(result.violations.length)} violations`,
  );
  const empty = result.scans.some((scan) => scan.scanned === 0);
  return result.violations.length === 0 && !empty ? 0 : 1;
};

if (import.meta.filename === path.resolve(process.argv[1] ?? '')) {
  process.exitCode = main();
}
