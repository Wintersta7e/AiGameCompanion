import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  decodeEntities,
  isActionableLink,
  lex,
  renderAnswer,
  renderMode,
  toNodes,
  type MdNode,
} from './markdown.ts';

/** Every node kind the conversion may produce. */
const KINDS = [
  'para',
  'heading',
  'list',
  'item',
  'code',
  'quote',
  'rule',
  'table',
  'strong',
  'em',
  'del',
  'codeInline',
  'br',
  'text',
  'link',
];

/** Every answer text the tests below convert. */
const FIXTURES = [
  'a **b** *c* ~~d~~ `e`',
  '1. x\n2. y',
  '3. x',
  '- [ ] t',
  '- [x] u',
  '```\nlet x',
  '<script>alert(1)</script>',
  '<img src=x onerror=alert(1)>',
  '![m](https://e.example/p.png)',
  'x<br>y',
  'x<BR />y',
  'Tom &amp; Jerry',
  '&copy;',
  '&amp;lt;',
  '`a &amp; b`',
  '## H',
  '| a | b |\n|---|---|\n| 1 | 2 |',
  '> q',
  '---',
  'see [docs](https://a.example) or www.a.example',
  '- a\n\n- b\n\n  > c',
];

const convert = (content: string): MdNode[] => toNodes(lex(content));

/** The children of the one paragraph `content` converts to. */
function paraChildren(content: string): MdNode[] {
  const [node, ...rest] = convert(content);
  assert.equal(rest.length, 0, content);
  assert.ok(node?.kind === 'para', content);
  return node.children;
}

/** The one list `content` converts to. */
function list(content: string): Extract<MdNode, { kind: 'list' }> {
  const [node, ...rest] = convert(content);
  assert.equal(rest.length, 0, content);
  assert.ok(node?.kind === 'list', content);
  return node;
}

const kinds = (nodes: MdNode[]): string[] => nodes.map((node) => node.kind);

const textOf = (nodes: MdNode[]): string =>
  nodes.map((node) => (node.kind === 'text' ? node.text : '')).join('');

/** The nodes directly inside `node`. */
function childrenOf(node: MdNode): MdNode[] {
  switch (node.kind) {
    case 'list':
      return node.items;
    case 'table':
      return [...node.header.flat(), ...node.rows.flat(2)];
    case 'para':
    case 'heading':
    case 'item':
    case 'quote':
    case 'strong':
    case 'em':
    case 'del':
    case 'link':
      return node.children;
    case 'code':
    case 'rule':
    case 'codeInline':
    case 'br':
    case 'text':
      return [];
  }
}

/** Each node of `nodes`, at any depth. */
const walk = (nodes: MdNode[]): MdNode[] =>
  nodes.flatMap((node) => [node, ...walk(childrenOf(node))]);

void test('toNodes: inline marks convert in order', () => {
  assert.deepEqual(kinds(paraChildren('a **b** *c* ~~d~~ `e`')), [
    'text',
    'strong',
    'text',
    'em',
    'text',
    'del',
    'text',
    'codeInline',
  ]);
});

void test('toNodes: lists keep their order, start and task boxes', () => {
  const numbered = list('1. x\n2. y');
  assert.equal(numbered.ordered, true);
  assert.equal(numbered.start, 1);
  assert.deepEqual(kinds(numbered.items), ['item', 'item']);
  assert.equal(list('3. x').start, 3);
  assert.equal(textOf(list('- [ ] t').items[0]?.children ?? []), '[ ] t');
  assert.equal(textOf(list('- [x] u').items[0]?.children ?? []), '[x] u');
});

void test('toNodes: an unclosed fence is a code block', () => {
  assert.deepEqual(convert('```\nlet x'), [{ kind: 'code', text: 'let x' }]);
});

void test('toNodes: raw HTML and images stay literal text', () => {
  const script = '<script>alert(1)</script>';
  assert.deepEqual(convert(script), [{ kind: 'text', text: script }]);
  const img = '<img src=x onerror=alert(1)>';
  assert.deepEqual(convert(img), [{ kind: 'text', text: img }]);
  const image = '![m](https://e.example/p.png)';
  assert.deepEqual(paraChildren(image), [{ kind: 'text', text: image }]);
});

void test('toNodes: an inline br tag is a line break', () => {
  assert.deepEqual(kinds(paraChildren('x<br>y')), ['text', 'br', 'text']);
  assert.deepEqual(kinds(paraChildren('x<BR />y')), ['text', 'br', 'text']);
});

void test('toNodes: entities decode once, code spans not at all', () => {
  assert.equal(textOf(paraChildren('Tom &amp; Jerry')), 'Tom & Jerry');
  assert.equal(textOf(paraChildren('&copy;')), '&copy;');
  assert.equal(textOf(paraChildren('&amp;lt;')), '&lt;');
  assert.equal(decodeEntities('&amp;lt; &quot;&#39;&nbsp;'), '&lt; "\' ');
  assert.deepEqual(paraChildren('`a &amp; b`'), [{ kind: 'codeInline', text: 'a &amp; b' }]);
});

void test('toNodes: an unknown token type becomes its raw text', () => {
  assert.deepEqual(toNodes([{ type: 'zzz', raw: 'Q' }]), [{ kind: 'text', text: 'Q' }]);
});

void test('toNodes: headings, tables, quotes and rules', () => {
  assert.deepEqual(kinds(convert('## H')), ['heading']);
  const [table] = convert('| a | b |\n|---|---|\n| 1 | 2 |');
  assert.ok(table?.kind === 'table');
  assert.equal(table.header.length, 2);
  assert.deepEqual(table.header.map(textOf), ['a', 'b']);
  assert.deepEqual(
    table.rows.map((row) => row.map(textOf)),
    [['1', '2']],
  );
  assert.deepEqual(kinds(convert('> q')), ['quote']);
  assert.deepEqual(convert('---'), [{ kind: 'rule' }]);
});

void test('toNodes: every fixture converts to the closed set of kinds', (t) => {
  const outputs = [...FIXTURES.map(convert), toNodes([{ type: 'zzz', raw: 'Q' }])];
  const nodes = outputs.flatMap(walk);
  const unknown = nodes.filter((node) => !KINDS.includes(node.kind)).map((node) => node.kind);
  t.diagnostic(`fixtures ${String(outputs.length)}, nodes ${String(nodes.length)}`);
  assert.deepEqual(unknown, []);
  assert.deepEqual(
    KINDS.filter((kind) => !nodes.some((node) => node.kind === kind)),
    [],
    'a kind no fixture produced',
  );
});

void test('isActionableLink: only a complete https link opens', () => {
  const [, autolink] = paraChildren('see www.a.example');
  assert.ok(autolink?.kind === 'link');
  assert.equal(autolink.href, 'http://www.a.example');
  assert.equal(isActionableLink('https://a.example', true), true);
  for (const [href, complete] of [
    ['https://a.example', false],
    ['javascript:alert(1)', true],
    ['http://a.example', true],
    [autolink.href, true],
    ['data:text/html,x', true],
    ['not a url', true],
  ] as const) {
    assert.equal(
      isActionableLink(href, complete),
      false,
      `${href} (complete: ${String(complete)})`,
    );
  }
});

void test('renderMode: long, slow or failed renders stay plain', () => {
  assert.equal(renderMode('markdown', 65_536, 0, false), 'markdown');
  assert.equal(renderMode('markdown', 65_537, 0, false), 'plain');
  assert.equal(renderMode('markdown', 1, 0, true), 'plain');
  assert.equal(renderMode('markdown', 1, 51, false), 'plain');
  assert.equal(renderMode('markdown', 1, 50, false), 'markdown');
  assert.equal(renderMode('plain', 1, 0, false), 'plain');
});

void test('renderAnswer: plain past the size cap, markdown below it', () => {
  assert.deepEqual(renderAnswer('x'.repeat(65_537), 'markdown'), { mode: 'plain', nodes: [] });
  const answer = renderAnswer('**b**', 'markdown');
  assert.equal(answer.mode, 'markdown');
  assert.deepEqual(kinds(answer.nodes), ['para']);
  assert.deepEqual(renderAnswer('**b**', 'plain'), { mode: 'plain', nodes: [] });
});
