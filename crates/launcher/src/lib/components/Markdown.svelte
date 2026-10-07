<script lang="ts">
  import { openLink } from '../utils/links';
  import { isActionableLink, renderAnswer, type MdNode } from '../utils/markdown';

  interface Props {
    content: string;
    streaming: boolean;
    complete: boolean;
    // Lives on the message, not here: the list reuses instances by index.
    plain: boolean;
    onplain: () => void;
  }

  type MdList = Extract<MdNode, { kind: 'list' }>;

  let { content, streaming, complete, plain, onplain }: Props = $props();

  const view = $derived(renderAnswer(content, plain ? 'plain' : 'markdown'));
  const caret = $derived(streaming && content !== '');
  // The caret ends the last paragraph or list item; after any other block it
  // follows the block.
  const caretInside = $derived(
    view.nodes.at(-1)?.kind === 'para' || view.nodes.at(-1)?.kind === 'list',
  );

  $effect(() => {
    if (view.mode === 'plain' && !plain) onplain();
  });
</script>

<!-- eslint-disable @typescript-eslint/no-confusing-void-expression -- {@render} of a snippet reads as a void call to this rule -->

{#snippet block(node: MdNode, withCaret: boolean)}
  {#if node.kind === 'para'}
    <p>
      {@render inline(node.children)}{#if withCaret}<span class="caret-blink"></span>{/if}
    </p>
  {:else if node.kind === 'heading'}
    <p class="md-heading">{@render inline(node.children)}</p>
  {:else if node.kind === 'list'}
    {#if node.ordered}
      <ol start={node.start}>{@render items(node, withCaret)}</ol>
    {:else}
      <ul>{@render items(node, withCaret)}</ul>
    {/if}
  {:else if node.kind === 'item'}
    <li>{@render inline(node.children)}</li>
  {:else if node.kind === 'code'}
    <pre><code>{node.text}</code></pre>
  {:else if node.kind === 'quote'}
    <blockquote>{@render inline(node.children)}</blockquote>
  {:else if node.kind === 'rule'}
    <hr />
  {:else if node.kind === 'table'}
    <div class="md-table">
      <table>
        <thead>
          <tr>
            {#each node.header as cell, c (c)}
              <th>{@render inline(cell)}</th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each node.rows as row, r (r)}
            <tr>
              {#each row as cell, c (c)}
                <td>{@render inline(cell)}</td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else}
    {@render inline([node])}
  {/if}
{/snippet}

{#snippet items(list: MdList, withCaret: boolean)}
  {#each list.items as item, i (i)}
    <li>
      {@render inline(item.children)}{#if withCaret && i === list.items.length - 1}<span
          class="caret-blink"
        ></span>{/if}
    </li>
  {/each}
{/snippet}

{#snippet inline(nodes: MdNode[])}
  {#each nodes as node, i (i)}
    {#if node.kind === 'text'}{node.text}{:else if node.kind === 'strong'}<strong
        >{@render inline(node.children)}</strong
      >{:else if node.kind === 'em'}<em>{@render inline(node.children)}</em
      >{:else if node.kind === 'del'}<del>{@render inline(node.children)}</del
      >{:else if node.kind === 'codeInline'}<code>{node.text}</code>{:else if node.kind === 'br'}<br
      />{:else if node.kind === 'link'}{#if isActionableLink(node.href, complete)}<button
          class="md-link"
          onclick={() => {
            openLink(node.href);
          }}
          title={node.href}
          type="button">{@render inline(node.children)}</button
        >{:else}<span class="md-link inert">{@render inline(node.children)}</span
        >{/if}{:else}{@render block(node, false)}{/if}
  {/each}
{/snippet}

{#if view.mode === 'plain'}
  <div class="md-plain">
    {content}{#if caret}<span class="caret-blink"></span>{/if}
  </div>
{:else}
  <div class="md">
    {#each view.nodes as node, i (i)}
      {@render block(node, caret && i === view.nodes.length - 1)}
    {/each}
    {#if caret && !caretInside}<span class="caret-blink"></span>{/if}
  </div>
{/if}

<style>
  .md {
    white-space: normal;
    min-width: 0;
  }
  .md > :first-child {
    margin-top: 0;
  }
  .md > :last-child {
    margin-bottom: 0;
  }
  p {
    margin: 0 0 8px;
  }
  .md-heading {
    /* Every depth alike, one pixel over the body text. */
    font-weight: 650;
    font-size: 14.5px;
    margin: 10px 0 6px;
  }
  ul,
  ol {
    margin: 0 0 8px;
    padding-left: 20px;
  }
  /* Tailwind's preflight sets list-style: none on every list. */
  ul {
    list-style: disc;
  }
  ol {
    list-style: decimal;
  }
  li {
    margin: 2px 0;
  }
  li::marker {
    color: var(--accent);
  }
  strong {
    font-weight: 650;
    color: var(--color-t-hi);
  }
  code {
    font-family: var(--font-mono);
    font-size: 12px;
    padding: 1px 5px;
    border-radius: 5px;
    background: var(--color-ink-3);
    border: 1px solid var(--color-line);
  }
  pre {
    margin: 0 0 8px;
    padding: 9px 11px;
    border-radius: 9px;
    background: var(--color-ink-1);
    border: 1px solid var(--color-line);
    white-space: pre;
    overflow-x: auto;
    max-width: 100%;
  }
  pre code {
    padding: 0;
    border: 0;
    border-radius: 0;
    background: none;
  }
  blockquote {
    margin: 0 0 8px;
    padding: 2px 0 2px 10px;
    border-left: 2px solid color-mix(in oklab, var(--accent) 55%, transparent);
    color: var(--color-t-mid);
  }
  hr {
    border: 0;
    border-top: 1px solid var(--color-line);
    margin: 10px 0;
  }
  .md-table {
    overflow-x: auto;
    max-width: 100%;
    margin: 0 0 8px;
  }
  table {
    border-collapse: collapse;
    font-size: 12.5px;
  }
  th,
  td {
    padding: 4px 8px;
    border: 1px solid var(--color-line);
    text-align: left;
    vertical-align: top;
  }
  th {
    background: var(--color-ink-3);
    font-weight: 600;
  }
  .md-link {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--accent);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .md-link:focus-visible {
    outline: 1px solid var(--accent);
    border-radius: 3px;
  }
  .md-link.inert {
    color: var(--color-t-mid);
    text-decoration-style: dotted;
    cursor: text;
  }
  .md-plain {
    white-space: pre-wrap;
  }
  .caret-blink {
    display: inline-block;
    width: 2px;
    height: 0.95em;
    background: var(--accent);
    margin-left: 2px;
    vertical-align: text-bottom;
    animation: blink 1s steps(2) infinite;
  }
  @keyframes blink {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0;
    }
  }
</style>
