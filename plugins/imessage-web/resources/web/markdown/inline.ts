/**
 * Inline marks: code spans, strong, emphasis, strikethrough, links, autolinks and bare URLs.
 * Whatever is not a mark is text, raw HTML included. Links open only `http(s):` and `mailto:`
 * targets, in a new tab; images become links named by their alt text, since a reply loads nothing.
 */
import { h } from "./dom";
import { INCOMPLETE_IMAGE, INCOMPLETE_LINK } from "./heal";

const PUNCTUATION = /[!-/:-@[-`{-~]/;
/**
 * A bare URL ends at whitespace, `<`, `>` or CJK punctuation, which a Chinese sentence puts right
 * after it (`https://example.com。谢谢`), as streamdown-cjk cuts GFM autolinks.
 */
const URL = /https?:\/\/[^\s<>。．，、？！：；（）【】「」『』〈〉《》]+/y;
const AUTOLINK = /<((?:https?:\/\/|mailto:)[^\s<>]+)>/y;
const SAFE_URL = /^(?:https?:\/\/|mailto:)/i;
const LETTER_OR_NUMBER = /[\p{L}\p{N}]/u;

/**
 * A link to `url` when it is one a reply may open; a link still streaming (healed to point at
 * {@link INCOMPLETE_LINK}) is drawn as a link without a target, so it looks the same when its URL
 * lands; any other URL leaves just its label.
 */
function link(url: string, label: Node[]): Node[] {
  if (url === INCOMPLETE_LINK || url === INCOMPLETE_IMAGE)
    return [
      h("a", { "data-incomplete": true, "aria-disabled": "true" }, label),
    ];
  if (!SAFE_URL.test(url)) return label;
  return [
    h("a", { href: url, target: "_blank", rel: "noopener noreferrer" }, label),
  ];
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
      const url = text
        .slice(close + 2, i)
        .trim()
        .replace(/\s+(?:"[^"]*"|'[^']*')$/, "")
        .replace(/^<(.*)>$/, "$1");
      return { end: i + 1, label: text.slice(start + 1, close), url };
    }
  }
  return null;
}

/**
 * The emphasis (`*`, `_`), strong (`**`, `__`) or strikethrough (`~~`) opened at `start`. An opener
 * is followed by non-whitespace and a closer preceded by it, with no punctuation clause, so CJK
 * punctuation next to a mark (`**中文。**`, `**“引用”**后`) works as remark-cjk-friendly makes it.
 */
function emphasisAt(text: string, start: number) {
  const c = text[start];
  let run = 1;
  while (text[start + run] === c) run += 1;
  if (c === "~" && run !== 2) return null;
  const size = run >= 2 ? 2 : 1;
  const after = text[start + run];
  if (after === undefined || /\s/.test(after)) return null;
  if (c === "_" && LETTER_OR_NUMBER.test(text[start - 1] ?? "")) return null;
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
      !(c === "_" && LETTER_OR_NUMBER.test(text[j + length] ?? ""))
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
export function inline(text: string): Node[] {
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
        // an image still streaming with no alt text yet draws nothing
        const name =
          found.label || (found.url === INCOMPLETE_IMAGE ? "" : found.url);
        const label = image
          ? name
            ? [document.createTextNode(name)]
            : []
          : inline(found.label);
        if (label.length) push(...link(found.url, label));
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
    if (c === "h" && !LETTER_OR_NUMBER.test(text[i - 1] ?? "")) {
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
