import { Lexer, type MarkedToken, type Token } from 'marked';

// Answer text is model output: it is lexed into tokens and converted into a
// closed set of nodes the component renders as elements and text. Nothing in
// this file produces HTML.

interface MdItem {
  kind: 'item';
  children: MdNode[];
}

export type MdNode =
  | { kind: 'para'; children: MdNode[] }
  | { kind: 'heading'; children: MdNode[] }
  | { kind: 'list'; ordered: boolean; start: number; items: MdItem[] }
  | MdItem
  | { kind: 'code'; text: string }
  | { kind: 'quote'; children: MdNode[] }
  | { kind: 'rule' }
  | { kind: 'table'; header: MdNode[][]; rows: MdNode[][][] }
  | { kind: 'strong'; children: MdNode[] }
  | { kind: 'em'; children: MdNode[] }
  | { kind: 'del'; children: MdNode[] }
  | { kind: 'codeInline'; text: string }
  | { kind: 'br' }
  | { kind: 'text'; text: string }
  | { kind: 'link'; href: string; children: MdNode[] };

type RenderMode = 'markdown' | 'plain';

/** Longer answers (UTF-16 units) render as plain text for the rest of their life. */
const MARKDOWN_MAX_CHARS = 65_536;
/** A lex slower than this switches its answer to plain text. */
const MAX_LEX_MS = 50;

const ENTITIES = new Map([
  ['amp', '&'],
  ['lt', '<'],
  ['gt', '>'],
  ['quot', '"'],
  ['#39', "'"],
  ['nbsp', ' '],
]);

/**
 * Decode the six entities marked leaves in text, in one pass, so `&amp;lt;`
 * becomes `&lt;` and not `<`. Any other entity stays literal.
 * @internal
 */
export function decodeEntities(text: string): string {
  return text.replace(
    /&(amp|lt|gt|quot|#39|nbsp);/gu,
    (entity, name: string) => ENTITIES.get(name) ?? entity,
  );
}

/** @internal */
export function lex(content: string): Token[] {
  return Lexer.lex(content, { gfm: true, breaks: true });
}

const BLOCK_TYPES = [
  'paragraph',
  'heading',
  'list',
  'list_item',
  'code',
  'blockquote',
  'hr',
  'table',
  'space',
  'def',
] as const;
type BlockToken = Extract<MarkedToken, { type: (typeof BLOCK_TYPES)[number] }>;
type InlineToken = Exclude<MarkedToken, BlockToken>;

const isBlock = (token: MarkedToken): token is BlockToken =>
  (BLOCK_TYPES as readonly string[]).includes(token.type);

function blockNodes(token: BlockToken): MdNode[] {
  switch (token.type) {
    case 'paragraph':
      return [{ kind: 'para', children: toNodes(token.tokens) }];
    case 'heading':
      return [{ kind: 'heading', children: toNodes(token.tokens) }];
    case 'list':
      return [
        {
          kind: 'list',
          ordered: token.ordered,
          start: token.start === '' ? 1 : token.start,
          items: token.items.map((item) => ({ kind: 'item', children: toNodes(item.tokens) })),
        },
      ];
    case 'list_item':
      return [{ kind: 'item', children: toNodes(token.tokens) }];
    case 'code':
      return [{ kind: 'code', text: token.text }];
    case 'blockquote':
      return [{ kind: 'quote', children: toNodes(token.tokens) }];
    case 'hr':
      return [{ kind: 'rule' }];
    case 'table':
      return [
        {
          kind: 'table',
          header: token.header.map((cell) => toNodes(cell.tokens)),
          rows: token.rows.map((row) => row.map((cell) => toNodes(cell.tokens))),
        },
      ];
    case 'space':
    case 'def':
      return [];
  }
}

function inlineNodes(token: InlineToken, raw: string): MdNode[] {
  switch (token.type) {
    case 'strong':
      return [{ kind: 'strong', children: toNodes(token.tokens) }];
    case 'em':
      return [{ kind: 'em', children: toNodes(token.tokens) }];
    case 'del':
      return [{ kind: 'del', children: toNodes(token.tokens) }];
    case 'codespan':
      return [{ kind: 'codeInline', text: token.text }];
    case 'br':
      return [{ kind: 'br' }];
    case 'text':
      return token.tokens
        ? toNodes(token.tokens)
        : [{ kind: 'text', text: decodeEntities(token.text) }];
    case 'escape':
      return [{ kind: 'text', text: decodeEntities(token.text) }];
    case 'checkbox':
      return [{ kind: 'text', text: token.checked ? '[x] ' : '[ ] ' }];
    case 'html':
      return !token.block && /^<br\s*\/?>$/iu.test(token.text)
        ? [{ kind: 'br' }]
        : [{ kind: 'text', text: token.text }];
    case 'link':
      return [{ kind: 'link', href: token.href, children: toNodes(token.tokens) }];
    case 'image':
      return [{ kind: 'text', text: token.raw }];
    default:
      // A token type this list does not know: its source text, literally.
      return [{ kind: 'text', text: raw }];
  }
}

/**
 * Convert marked's tokens into nodes of the closed set above. Total: every
 * token yields nodes, and an unknown token yields its source text.
 * @internal
 */
export function toNodes(tokens: Token[]): MdNode[] {
  return tokens.flatMap((token) => {
    const known = token as MarkedToken;
    return isBlock(known) ? blockNodes(known) : inlineNodes(known, token.raw);
  });
}

/** Whether a link may open: only once the answer is complete, and only https. */
export function isActionableLink(href: string, complete: boolean): boolean {
  if (!complete) return false;
  try {
    return new URL(href).protocol === 'https:';
  } catch {
    return false;
  }
}

/**
 * How an answer renders. Once plain it stays plain, so a slow or failed lex
 * never flips back and forth while an answer streams.
 * @internal
 */
export function renderMode(
  previous: RenderMode,
  length: number,
  lexMs: number,
  threw: boolean,
): RenderMode {
  return previous === 'plain' || length > MARKDOWN_MAX_CHARS || threw || lexMs > MAX_LEX_MS
    ? 'plain'
    : 'markdown';
}

/** The nodes to render for `content`, or none when it renders as plain text. */
export function renderAnswer(
  content: string,
  previous: RenderMode,
): { mode: RenderMode; nodes: MdNode[] } {
  if (renderMode(previous, content.length, 0, false) === 'plain') {
    return { mode: 'plain', nodes: [] };
  }
  try {
    const started = performance.now();
    const tokens = lex(content);
    const mode = renderMode(previous, content.length, performance.now() - started, false);
    return { mode, nodes: mode === 'plain' ? [] : toNodes(tokens) };
  } catch {
    return { mode: renderMode(previous, content.length, 0, true), nodes: [] };
  }
}
