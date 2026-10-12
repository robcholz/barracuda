/**
 * Streaming Markdown for chat replies, framework-free and small enough to ship from a device: the
 * rendering logic of Vercel's Streamdown, rebuilt on the DOM. See README.md.
 *
 * {@link renderMarkdown} paints a reply into an element and paints it again as it grows:
 *
 * - The text is healed ({@link heal}, ported from Streamdown's remend) so that an open mark, code
 *   span or link renders as it will once closed; while it streams, the last line also waits until
 *   its block is known ({@link shown}). Nothing shown is taken back as the text grows.
 * - The reply is split into top-level blocks, and a block whose source is unchanged keeps its node,
 *   as Streamdown memoizes its blocks; the block that grew is patched in place ({@link morph}),
 *   keeping its elements and text nodes, so a selection, a hovered button and a code block's scroll
 *   survive every paint.
 * - Nodes are built, never parsed from markup, so HTML in a reply stays text.
 */
import { segments, type Segment } from "./blocks";
import { h, morph, sync } from "./dom";
import { highlight } from "./highlight";
import { inline } from "./inline";
import { shown } from "./stream";

export { heal } from "./heal";
export { highlight } from "./highlight";
export { MARKDOWN_CSS } from "./style";

export interface MarkdownOptions {
  /** The actions on a code block's head (its Copy button), given a reader of the block's code. */
  codeActions?: (read: () => string) => Node;
  /** The text is still streaming: its tail is shown provisionally, an open fence as incomplete. */
  streaming?: boolean;
  /** A node of the page's own (the caret) that painting leaves where it is. */
  keep?: Node;
}

interface Painted {
  source: string;
  blocks: { key: string; node: HTMLElement }[];
}

const painted = new WeakMap<HTMLElement, Painted>();
const TASK = /^\[([ xX])\][ \t]+/;
/** Elements whose last line a trailing node (the caret, an 「已编辑」 note) joins. */
const FLOWING = /^(?:P|LI|UL|OL|BLOCKQUOTE|H[1-6])$/;

/**
 * Paints `source` into `target` over what an earlier paint drew there: a top-level block whose
 * source is unchanged keeps its node, and one that changed is patched in place.
 */
export function renderMarkdown(
  target: HTMLElement,
  source: string,
  options: MarkdownOptions = {},
) {
  const lines = shown(
    source.replace(/\r\n?/g, "\n"),
    !!options.streaming,
  ).split("\n");
  const previous = painted.get(target)?.blocks ?? [];
  const blocks = segments(lines).map((segment, index) => {
    const incomplete =
      segment.kind === "code" && segment.open && !!options.streaming;
    // a code block that stops streaming is painted again, even with the same source
    const key = `${incomplete ? "…" : ""}${lines.slice(segment.from, segment.to).join("\n")}`;
    const old = previous[index];
    if (old?.key === key) return old;
    const node = build(segment, options, incomplete);
    return {
      key,
      node: old ? (morph(old.node, node, options.keep) as HTMLElement) : node,
    };
  });
  sync(
    target,
    blocks.map((block) => block.node),
    options.keep,
  );
  painted.set(target, { source, blocks });
}

/** The Markdown last painted into `target`, as Copy and Reply take it. */
export function markdownSource(target: Element) {
  return painted.get(target as HTMLElement)?.source;
}

/**
 * The element whose last line ends the painted text: where a caret or an 「已编辑」 note goes. It is
 * `target` itself when the text ends in a block with no last line (a code block, a table, a rule),
 * where Streamdown hides its caret. `skip` (the caret itself) is passed over.
 */
export function trailing(target: HTMLElement, skip?: Node) {
  const last = (node: Element) =>
    node.lastElementChild === skip
      ? node.lastElementChild?.previousElementSibling
      : node.lastElementChild;
  let node = target;
  for (
    let next = last(node);
    next && FLOWING.test(next.tagName);
    next = last(node)
  )
    node = next as HTMLElement;
  return node;
}

function blocks(lines: string[], options: MarkdownOptions) {
  return segments(lines).map((segment) => build(segment, options, false));
}

function build(
  segment: Segment,
  options: MarkdownOptions,
  incomplete: boolean,
): HTMLElement {
  switch (segment.kind) {
    case "code": {
      const { lang, body } = segment;
      const pre = h(
        "pre",
        null,
        h("code", null, (lang && highlight(body, lang)) || body),
      );
      // while its fence is open the block is incomplete, as Streamdown marks it: its actions rest
      return h(
        "div",
        {
          class: "bc-md-code",
          "data-language": lang || null,
          "data-incomplete": incomplete,
        },
        h(
          "div",
          { class: "bc-md-code__head" },
          h("span", null, lang || "text"),
          options.codeActions &&
            h(
              "span",
              { class: "bc-md-code__actions", "data-md-keep": true },
              options.codeActions(() => pre.textContent ?? ""),
            ),
        ),
        pre,
      );
    }
    case "heading":
      return h(`h${segment.level}`, null, inline(segment.text));
    case "rule":
      return h("hr");
    case "quote":
      return h("blockquote", null, blocks(segment.body, options));
    case "table": {
      const cell = (tag: string, text: string, i: number) =>
        h(tag, { style: { textAlign: segment.align[i] } }, inline(text));
      return h(
        "div",
        { class: "bc-md-table" },
        h(
          "table",
          null,
          h(
            "thead",
            null,
            h(
              "tr",
              null,
              segment.head.map((text, i) => cell("th", text, i)),
            ),
          ),
          h(
            "tbody",
            null,
            segment.rows.map((row) =>
              h(
                "tr",
                null,
                // the header sets the columns: a short row is padded, a long one cut
                segment.head.map((_, i) => cell("td", row[i] ?? "", i)),
              ),
            ),
          ),
        ),
      );
    }
    case "list":
      return h(
        segment.ordered ? "ol" : "ul",
        { start: segment.start !== 1 ? segment.start : null },
        segment.items.map((body) => {
          const task = TASK.exec(body[0]);
          if (task) body = [body[0].slice(task[0].length), ...body.slice(1)];
          const content = blocks(body, options);
          if (!task) return h("li", null, content);
          const box = h("input", {
            type: "checkbox",
            disabled: true,
            checked: task[1] !== " ",
          });
          if (content[0]?.tagName === "P") content[0].prepend(box, " ");
          else content.unshift(h("p", null, box));
          return h("li", { class: "bc-md-task" }, content);
        }),
      );
    case "paragraph":
      return h("p", null, inline(segment.text));
  }
}
