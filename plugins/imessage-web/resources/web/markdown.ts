/**
 * The Agent's replies as Markdown: the CommonMark and GFM blocks a model writes (paragraphs,
 * headings, fenced code, lists and task lists, quotes, rules and tables) and their inline marks
 * (code, strong, emphasis, strikethrough, links). It builds DOM nodes and never parses markup, so
 * HTML in a reply stays text; links open only `http(s):` and `mailto:` targets, in a new tab.
 *
 * It is written for streaming, as Streamdown is: the reply is parsed again as its text grows, and
 * what is already on screen never flashes.
 *
 * - While a reply streams, its tail is made provisional ({@link provisional}): an open mark, code
 *   span or link is closed (`**bol` shows bold, `[文档](https://ex` its label), a mark run at the very
 *   end waits, a last line that is only a block's marker (`#`, `-`, `1.`, `` ` ``) waits to see which
 *   block it starts, a table's header row waits for its delimiter row, and a code fence's own line
 *   and closing backticks wait. What has been shown is not taken back as the text grows.
 * - Painting patches the DOM in place ({@link morph}): a top-level block whose source is unchanged
 *   keeps its node, and the block that grew keeps its elements and text nodes, its text appended, so
 *   a selection, a hovered Copy button and a code block's scroll survive every frame.
 */
import { h } from "../../../captive-portal/resources/web/ui";
import { highlight } from "./highlight";

export interface MarkdownOptions {
  /** The actions on a code block's head (its Copy button), given a reader of the block's code. */
  codeActions?: (read: () => string) => Node;
  /** The text is still streaming: its tail is shown provisionally. */
  streaming?: boolean;
  /** A node of the page's own (the caret) that painting leaves where it is. */
  keep?: Node;
}

type Segment = { from: number; to: number } & (
  | { kind: "code"; lang: string; body: string }
  | { kind: "heading"; level: number; text: string }
  | { kind: "rule" }
  | { kind: "quote"; body: string[] }
  | { kind: "table"; head: string[]; align: string[]; rows: string[][] }
  | { kind: "list"; ordered: boolean; start: number; items: string[][] }
  | { kind: "paragraph"; text: string }
);

interface Painted {
  source: string;
  blocks: { raw: string; node: HTMLElement }[];
}

const painted = new WeakMap<HTMLElement, Painted>();
const ELEMENT_NODE = 1;
const TEXT_NODE = 3;

const FENCE = /^( {0,3})(`{3,}|~{3,})[ \t]*([^\s`]*)/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}> ?/;
const ITEM = /^( *)([-*+]|\d{1,9}[.)])(?:[ \t]+|$)/;
const DELIMITER = /^ *\|? *:?-+:? *(?:\| *:?-+:? *)*\|? *$/;
const TASK = /^\[([ xX])\][ \t]+/;
const PUNCTUATION = /[!-/:-@[-`{-~]/;
const URL = /https?:\/\/[^\s<>]+/y;
const AUTOLINK = /<((?:https?:\/\/|mailto:)[^\s<>]+)>/y;
const SAFE_URL = /^(?:https?:\/\/|mailto:)/i;
/** Elements whose last line a trailing node (the caret, 「已编辑」) joins. */
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
  const text = source.replace(/\r\n?/g, "\n");
  const lines = (options.streaming ? provisional(text) : text).split("\n");
  const previous = painted.get(target)?.blocks ?? [];
  const blocks = segments(lines).map((segment, index) => {
    const raw = lines.slice(segment.from, segment.to).join("\n");
    const old = previous[index];
    if (old?.raw === raw) return old;
    const node = build(segment, options);
    return {
      raw,
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

/**
 * Makes `node` what `next` is, keeping `node`'s elements and text nodes wherever `next` has the same
 * kind in the same place; returns the node to use (`next` when the kinds differ). An element marked
 * `data-md-keep` (a code block's actions) is left as it is.
 */
function morph(node: Node, next: Node, keep?: Node): Node {
  if (node.nodeName !== next.nodeName) return next;
  if (node.nodeType === TEXT_NODE) {
    const text = node as Text;
    const { data } = next as Text;
    // appending keeps a selection inside the text
    if (data.startsWith(text.data))
      text.appendData(data.slice(text.data.length));
    else text.data = data;
    return node;
  }
  const element = node as Element;
  if (node.nodeType !== ELEMENT_NODE || element.hasAttribute("data-md-keep"))
    return node;
  const model = next as Element;
  for (const { name } of [...element.attributes])
    if (!model.hasAttribute(name)) element.removeAttribute(name);
  for (const { name, value } of [...model.attributes])
    if (element.getAttribute(name) !== value) element.setAttribute(name, value);
  const children = [...node.childNodes].filter((child) => child !== keep);
  sync(
    node,
    [...model.childNodes].map((child, i) =>
      children[i] ? morph(children[i], child, keep) : child,
    ),
    keep,
  );
  return node;
}

/** Puts `children` in `parent` in order, moving only what is out of place; new ones go before `keep`. */
function sync(parent: Node, children: Node[], keep?: Node) {
  let at = parent.firstChild;
  for (const child of children) {
    if (at === child) at = at.nextSibling;
    else parent.insertBefore(child, at);
  }
  while (at) {
    const next = at.nextSibling;
    if (at !== keep) parent.removeChild(at);
    at = next;
  }
}

/** A last line that is only a block's marker: which block it starts is not known yet. */
const MARKER_ONLY =
  /^[ \t]*(?:#{1,6}|>|`{1,2}|~{1,2}|\d{1,9}[.)]?|[-*+_](?:[ \t]*[-*_])?|(?:[-*+]|\d{1,9}[.)])[ \t]+\[(?:[ xX]\]?)?)[ \t]*$/;
/** The block marks ahead of a line's inline text. */
const LINE_PREFIX =
  /^([ \t]*(?:>[ \t]?)*(?:#{1,6}[ \t]+|(?:[-*+]|\d{1,9}[.)])[ \t]+(?:\[[ xX]\][ \t]+)?)?)(.*)$/;
const TABLE_ROW = /^[ \t]*\|/;

/**
 * What a reply still streaming shows: `text` with its tail settled so that nothing shown is taken
 * back as more arrives (see the module comment).
 */
function provisional(text: string) {
  const lines = text.split("\n");
  let last = lines.length - 1;
  let fence = -1;
  for (let i = 0; i < lines.length; i += 1) {
    const marks = /^[ \t]*(`{3,}|~{3,})/.exec(lines[i]);
    if (!marks) continue;
    if (fence < 0) fence = i;
    else if (
      /^[ \t]*(?:`{3,}|~{3,})[ \t]*$/.test(lines[i]) &&
      marks[1][0] === lines[fence].trimStart()[0]
    )
      fence = -1;
  }
  if (fence >= 0) {
    // the fence's line until its language is whole; a closing fence's first backticks
    if (fence === last || /^[ \t]*(?:`+|~+)$/.test(lines[last])) lines.pop();
    return lines.join("\n");
  }
  // a line just ended draws nothing yet; what it ended is still the tail
  if (last > 0 && !lines[last]) {
    lines.pop();
    last -= 1;
  }
  const line = lines[last];
  const above = lines[last - 1] ?? "";
  if (MARKER_ONLY.test(line)) {
    lines.pop();
    last -= 1;
  } else if (TABLE_ROW.test(line) && !above.includes("|")) {
    // a header row waits for its delimiter row
    lines.pop();
    last -= 1;
  } else if (
    /^[ \t]*\|[ \t|:-]*$/.test(line) &&
    TABLE_ROW.test(above) &&
    !(lines[last - 2] ?? "").includes("|")
  ) {
    // a delimiter row on its way already makes the table, with the columns its header has
    const typed = cells(line);
    lines[last] = `|${cells(above)
      .map((_, i) => (/^:?-+:?$/.test(typed[i] ?? "") ? typed[i] : "---"))
      .join("|")}|`;
    return lines.join("\n");
  }
  if (last >= 0 && !RULE.test(lines[last])) {
    const [, prefix, body] = LINE_PREFIX.exec(lines[last])!;
    lines[last] = prefix + closeInline(body);
  }
  return lines.join("\n");
}

/**
 * Closes what a line's text leaves open at its end: a code span, a link (shown as its label until its
 * URL is whole) and the marks still open, innermost first. A mark run at the very end, which could
 * yet open, close or be text, is held back.
 */
function closeInline(text: string) {
  let scan = openMarks(text);
  if (!scan.code) {
    // a link first, so the marks are read on the text that will show
    const linked = closeLink(text);
    if (linked !== text) {
      text = linked;
      scan = openMarks(text);
    }
  }
  const [, core, space] = /^([\s\S]*?)(\s*)$/.exec(text.slice(0, scan.end))!;
  return core + scan.code + scan.open.reverse().join("") + space;
}

/**
 * A link still arriving: its URL closed with as many `)` as it leaves open, or, before its URL, its
 * label without the brackets.
 */
function closeLink(text: string) {
  const at = text.lastIndexOf("](");
  if (at >= 0 && text.lastIndexOf("[", at) >= 0) {
    let depth = 1;
    for (let i = at + 2; i < text.length && depth > 0; i += 1) {
      if (/\s/.test(text[i])) return text;
      if (text[i] === "(") depth += 1;
      else if (text[i] === ")") depth -= 1;
    }
    if (depth > 0) return text + ")".repeat(depth);
  }
  return text.replace(/\[([^\]]*)\]?$/, "$1");
}

/**
 * Reads a line's inline text for what its end leaves open: the mark runs still open, in order, a code
 * span's backticks, and where a run at the very end begins (`end`), which is held back.
 */
function openMarks(text: string) {
  const open: string[] = [];
  let end = text.length;
  let code = "";
  for (let i = 0; i < text.length;) {
    const c = text[i];
    if (c === "\\") {
      i += 2;
      continue;
    }
    let run = 1;
    while (text[i + run] === c) run += 1;
    if (c === "`") {
      const close = closingTicks(text, i, run);
      if (close >= 0) {
        i = close + run;
        continue;
      }
      if (i + run === text.length) end = i;
      else code = c.repeat(run);
      break;
    }
    if (c !== "*" && c !== "_" && c !== "~") {
      i += 1;
      continue;
    }
    const before = text[i - 1];
    if (open.at(-1)?.[0] === c && before && !/\s/.test(before)) {
      let left = run;
      while (left > 0 && open.at(-1)?.[0] === c && open.at(-1)!.length <= left)
        left -= open.pop()!.length;
      if (left < run) {
        i += run;
        continue;
      }
    }
    const after = text[i + run];
    if (after === undefined) {
      end = i;
      break;
    }
    if (
      !/\s/.test(after) &&
      !(c === "_" && /[\p{L}\p{N}]/u.test(before ?? "")) &&
      (c !== "~" || run === 2)
    )
      open.push(c.repeat(run));
    i += run;
  }
  return { open, end, code };
}

/** The Markdown last painted into `target`, as Copy and Reply take it. */
export function markdownSource(target: Element) {
  return painted.get(target as HTMLElement)?.source;
}

/**
 * The element whose last line ends the painted text: where the caret or an 「已编辑」 note goes. `skip`
 * (the caret itself) is passed over.
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

const blank = (line: string) => !line.trim();
const indentOf = (line: string) => line.length - line.trimStart().length;

/** Whether `line` starts a block that ends a paragraph before it. */
function interrupts(lines: string[], i: number) {
  const line = lines[i];
  return (
    FENCE.test(line) ||
    HEADING.test(line) ||
    RULE.test(line) ||
    QUOTE.test(line) ||
    ITEM.test(line) ||
    tableAt(lines, i)
  );
}

function tableAt(lines: string[], i: number) {
  const next = lines[i + 1];
  return (
    lines[i].includes("|") &&
    next !== undefined &&
    next.includes("|") &&
    DELIMITER.test(next)
  );
}

function segments(lines: string[]): Segment[] {
  const out: Segment[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (blank(line)) {
      i += 1;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      const [, pad, ticks, lang] = fence;
      const body: string[] = [];
      let j = i + 1;
      for (; j < lines.length; j += 1) {
        const close = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(lines[j]);
        if (
          close &&
          close[1][0] === ticks[0] &&
          close[1].length >= ticks.length
        )
          break;
        // the fence's own indent comes off each line
        body.push(lines[j].slice(Math.min(pad.length, indentOf(lines[j]))));
      }
      const to = Math.min(j + 1, lines.length);
      out.push({
        kind: "code",
        from: i,
        to,
        lang: lang.split(/[,{]/)[0],
        body: body.join("\n"),
      });
      i = to;
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      out.push({
        kind: "heading",
        from: i,
        to: i + 1,
        level: heading[1].length,
        text: heading[2] ?? "",
      });
      i += 1;
      continue;
    }
    if (RULE.test(line)) {
      out.push({ kind: "rule", from: i, to: i + 1 });
      i += 1;
      continue;
    }
    if (QUOTE.test(line)) {
      const body: string[] = [];
      let j = i;
      for (; j < lines.length; j += 1) {
        if (QUOTE.test(lines[j])) body.push(lines[j].replace(QUOTE, ""));
        // a lazy line carries the quote's paragraph on
        else if (
          !blank(lines[j]) &&
          !interrupts(lines, j) &&
          !blank(body.at(-1)!)
        )
          body.push(lines[j]);
        else break;
      }
      out.push({ kind: "quote", from: i, to: j, body });
      i = j;
      continue;
    }
    if (ITEM.test(line)) {
      const list = listAt(lines, i);
      out.push(list);
      i = list.to;
      continue;
    }
    if (tableAt(lines, i)) {
      const head = cells(line);
      const align = cells(lines[i + 1]).map((cell) =>
        cell.endsWith(":") ? (cell.startsWith(":") ? "center" : "right") : "",
      );
      const rows: string[][] = [];
      let j = i + 2;
      for (; j < lines.length && lines[j].includes("|"); j += 1)
        rows.push(cells(lines[j]));
      out.push({ kind: "table", from: i, to: j, head, align, rows });
      i = j;
      continue;
    }
    const text: string[] = [];
    let j = i;
    for (; j < lines.length && !blank(lines[j]); j += 1) {
      if (j > i && interrupts(lines, j)) break;
      text.push(lines[j].trim().replace(/\\$/, ""));
    }
    out.push({ kind: "paragraph", from: i, to: j, text: text.join("\n") });
    i = j;
  }
  return out;
}

/**
 * A list from its first item at `start`: items at the same indent and of the same kind, each with
 * the lines indented past its marker (nested lists, more paragraphs) and the lazy lines of its
 * paragraph. Any line indented past the list's own marker belongs to the item, however far, since
 * a model nests with two spaces as often as with the marker's width.
 */
function listAt(lines: string[], start: number): Segment & { kind: "list" } {
  const first = ITEM.exec(lines[start])!;
  const indent = first[1].length;
  const ordered = /\d/.test(first[2]);
  const items: string[][] = [];
  let i = start;
  let to = start;
  while (i < lines.length) {
    const item = ITEM.exec(lines[i]);
    if (
      !item ||
      RULE.test(lines[i]) ||
      item[1].length !== indent ||
      /\d/.test(item[2]) !== ordered
    )
      break;
    const width = item[0].length;
    const body = [lines[i].slice(width)];
    i += 1;
    to = i;
    while (i < lines.length) {
      const line = lines[i];
      if (blank(line)) {
        let k = i;
        while (k < lines.length && blank(lines[k])) k += 1;
        if (k === lines.length || indentOf(lines[k]) <= indent) break;
        for (; i < k; i += 1) body.push("");
        continue;
      }
      if (indentOf(line) > indent) {
        body.push(line.slice(Math.min(indentOf(line), width)));
      } else if (!interrupts(lines, i) && !blank(body.at(-1)!)) {
        body.push(line);
      } else break;
      i += 1;
      to = i;
    }
    items.push(body);
    // blank lines between two items of the list
    let k = i;
    while (k < lines.length && blank(lines[k])) k += 1;
    if (k > i) {
      const next = ITEM.exec(lines[k] ?? "");
      if (!next || next[1].length !== indent || /\d/.test(next[2]) !== ordered)
        break;
      i = k;
    }
  }
  return {
    kind: "list",
    from: start,
    to,
    ordered,
    start: ordered ? parseInt(first[2], 10) : 1,
    items,
  };
}

/** A table row's cells: split on `|` outside code spans, an escaped `\|` kept. */
function cells(line: string) {
  let row = line.trim();
  if (row.startsWith("|")) row = row.slice(1);
  if (row.endsWith("|") && !row.endsWith("\\|")) row = row.slice(0, -1);
  const out: string[] = [];
  let cell = "";
  let code = false;
  for (let i = 0; i < row.length; i += 1) {
    const c = row[i];
    if (c === "\\" && row[i + 1] === "|") {
      cell += "|";
      i += 1;
    } else if (c === "`") {
      code = !code;
      cell += c;
    } else if (c === "|" && !code) {
      out.push(cell.trim());
      cell = "";
    } else cell += c;
  }
  out.push(cell.trim());
  return out;
}

function blocks(lines: string[], options: MarkdownOptions) {
  return segments(lines).map((segment) => build(segment, options));
}

function build(segment: Segment, options: MarkdownOptions): HTMLElement {
  switch (segment.kind) {
    case "code": {
      const { lang, body } = segment;
      const pre = h(
        "pre",
        null,
        h("code", null, (lang && highlight(body, lang)) || body),
      );
      return h(
        "div",
        { class: "bc-md-code" },
        h(
          "div",
          { class: "bc-md-code__head" },
          h("span", null, lang || "text"),
          options.codeActions &&
            h(
              "span",
              { "data-md-keep": true, style: "display:contents" },
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
    case "table":
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
              segment.head.map((cell, i) =>
                h(
                  "th",
                  { style: { textAlign: segment.align[i] } },
                  inline(cell),
                ),
              ),
            ),
          ),
          h(
            "tbody",
            null,
            segment.rows.map((row) =>
              h(
                "tr",
                null,
                segment.head.map((_, i) =>
                  h(
                    "td",
                    { style: { textAlign: segment.align[i] } },
                    inline(row[i] ?? ""),
                  ),
                ),
              ),
            ),
          ),
        ),
      );
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

/** Where the code span opened by `run` backticks at `start` closes: a run of exactly as many, or -1. */
function closingTicks(text: string, start: number, run: number) {
  const ticks = "`".repeat(run);
  let close = text.indexOf(ticks, start + run);
  while (close >= 0 && text[close + run] === "`") {
    let next = close;
    while (text[next] === "`") next += 1;
    close = text.indexOf(ticks, next);
  }
  return close;
}

/** A link to `url` when it is one a reply may open, else just its label. */
function link(url: string, label: Node[]) {
  return SAFE_URL.test(url)
    ? [
        h(
          "a",
          { href: url, target: "_blank", rel: "noopener noreferrer" },
          label,
        ),
      ]
    : label;
}

/**
 * `[label](url "title")` from the `[` at `start`: where it ends, its label and its URL; null when
 * no link starts there.
 */
function linkAt(text: string, start: number) {
  let depth = 0;
  let close = -1;
  for (let i = start; i < text.length; i += 1) {
    if (text[i] === "\\") i += 1;
    else if (text[i] === "[") depth += 1;
    else if (text[i] === "]" && --depth === 0) {
      close = i;
      break;
    }
  }
  if (close < 0 || text[close + 1] !== "(") return null;
  depth = 0;
  for (let i = close + 1; i < text.length; i += 1) {
    if (text[i] === "\\") i += 1;
    else if (text[i] === "(") depth += 1;
    else if (text[i] === ")" && --depth === 0) {
      const target = text
        .slice(close + 2, i)
        .trim()
        .replace(/\s+(?:"[^"]*"|'[^']*')$/, "")
        .replace(/^<(.*)>$/, "$1");
      return { end: i + 1, label: text.slice(start + 1, close), url: target };
    }
  }
  return null;
}

/** The emphasis (`*`, `_`), strong (`**`, `__`) or strikethrough (`~~`) opened at `start`. */
function emphasisAt(text: string, start: number) {
  const c = text[start];
  let run = 1;
  while (text[start + run] === c) run += 1;
  if (c === "~" && run !== 2) return null;
  const size = run >= 2 ? 2 : 1;
  const after = text[start + run];
  if (after === undefined || /\s/.test(after)) return null;
  if (c === "_" && /[\p{L}\p{N}]/u.test(text[start - 1] ?? "")) return null;
  for (let j = start + run; j < text.length;) {
    j = text.indexOf(c, j);
    if (j < 0) return null;
    let length = 1;
    while (text[j + length] === c) length += 1;
    const closer = j + length - size;
    // emphasis passes over a strong's `**` inside it; strong passes over a lone `*`
    if (
      (size === 1 ? length !== 2 : length >= 2) &&
      closer > start + size &&
      !/\s/.test(text[j - 1]) &&
      !(c === "_" && /[\p{L}\p{N}]/u.test(text[j + length] ?? ""))
    ) {
      const tag = c === "~" ? "del" : size === 2 ? "strong" : "em";
      return {
        end: closer + size,
        node: h(tag, null, inline(text.slice(start + size, closer))),
      };
    }
    j += length;
  }
  return null;
}

/** A paragraph's inline marks as nodes; whatever is not a mark is text. */
function inline(text: string): Node[] {
  const out: Node[] = [];
  let plain = "";
  const push = (...nodes: Node[]) => {
    if (plain) out.push(document.createTextNode(plain));
    plain = "";
    out.push(...nodes);
  };
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === "\\" && PUNCTUATION.test(text[i + 1] ?? "")) {
      plain += text[i + 1];
      i += 2;
      continue;
    }
    if (c === "`") {
      let run = 1;
      while (text[i + run] === "`") run += 1;
      const close = closingTicks(text, i, run);
      if (close < 0) {
        plain += "`".repeat(run);
        i += run;
        continue;
      }
      let code = text.slice(i + run, close).replace(/\n/g, " ");
      if (/^ .*[^ ].* $/.test(code)) code = code.slice(1, -1);
      push(h("code", null, code));
      i = close + run;
      continue;
    }
    if (c === "[" || (c === "!" && text[i + 1] === "[")) {
      const image = c === "!";
      const found = linkAt(text, image ? i + 1 : i);
      if (found) {
        // an image is not loaded from the device's page: it becomes a link named by its alt text
        const label = image
          ? [document.createTextNode(found.label || found.url)]
          : inline(found.label);
        push(...link(found.url, label));
        i = found.end;
        continue;
      }
    }
    if (c === "<") {
      AUTOLINK.lastIndex = i;
      const found = AUTOLINK.exec(text);
      if (found) {
        push(...link(found[1], [document.createTextNode(found[1])]));
        i = AUTOLINK.lastIndex;
        continue;
      }
    }
    if (c === "h" && !/[\p{L}\p{N}]/u.test(text[i - 1] ?? "")) {
      URL.lastIndex = i;
      const found = URL.exec(text);
      if (found) {
        let url = found[0].replace(/[.,:;!?'"*_~]+$/, "");
        while (
          url.endsWith(")") &&
          url.split(")").length > url.split("(").length
        )
          url = url.slice(0, -1);
        push(...link(url, [document.createTextNode(url)]));
        i += url.length;
        continue;
      }
    }
    if (c === "*" || c === "_" || c === "~") {
      const found = emphasisAt(text, i);
      if (found) {
        push(found.node);
        i = found.end;
        continue;
      }
      // a run that opens nothing stays text as a whole, so its tail cannot open a mark
      let run = 1;
      while (text[i + run] === c) run += 1;
      plain += c.repeat(run);
      i += run;
      continue;
    }
    plain += c;
    i += 1;
  }
  push();
  return out;
}
