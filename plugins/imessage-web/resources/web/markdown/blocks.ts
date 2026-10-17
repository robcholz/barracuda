/**
 * Block structure: the lines of a reply as the CommonMark and GFM blocks a model writes. Lenient
 * where models are loose (a list nests with two spaces as readily as with its marker's width) and
 * without what models do not write (setext headings, indented code, HTML blocks, link definitions).
 */

export type Segment = { from: number; to: number } & (
  | { kind: "code"; lang: string; body: string; open: boolean }
  | { kind: "heading"; level: number; text: string }
  | { kind: "rule" }
  | { kind: "quote"; body: string[] }
  | { kind: "table"; head: string[]; align: string[]; rows: string[][] }
  | { kind: "list"; ordered: boolean; start: number; items: string[][] }
  | { kind: "paragraph"; text: string }
);

export const FENCE = /^( {0,3})(`{3,}|~{3,})[ \t]*([^\s`]*)/;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
export const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}> ?/;
const ITEM = /^( *)([-*+]|\d{1,9}[.)])(?:[ \t]+|$)/;
export const DELIMITER = /^ *\|? *:?-+:? *(?:\| *:?-+:? *)*\|? *$/;

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

/** The blocks of `lines`, each with the range of lines it spans. */
export function segments(lines: string[]): Segment[] {
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
        const close = FENCE_CLOSE.exec(lines[j]);
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
        open: j === lines.length,
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
 * paragraph. Any line indented past the list's own marker belongs to the item, however far.
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
      if (indentOf(line) > indent)
        body.push(line.slice(Math.min(indentOf(line), width)));
      else if (!interrupts(lines, i) && !blank(body.at(-1)!)) body.push(line);
      else break;
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
export function cells(line: string) {
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
