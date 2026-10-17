/**
 * Healing for Markdown that is still streaming: closes what the text so far leaves open, so a reply
 * renders as it will once whole instead of flashing its syntax (`**bol` → `**bol**`, `` `co `` →
 * `` `co` ``, `[label](https://ex` → a link still on its way).
 *
 * Ported from remend 1.4.0 (https://github.com/vercel/streamdown, packages/remend), Copyright Vercel,
 * Inc., under the Apache License 2.0; see NOTICE and APACHE-2.0.txt in this directory. Changes: the
 * handlers for KaTeX, HTML tags, setext headings, single tildes and custom handlers are left out,
 * as this renderer draws `$`, HTML and setext underlines as text and has no single-tilde
 * strikethrough; so are the math and HTML-tag masks that kept delimiters inside them from counting.
 * The modules are joined into one, without options. One fix: only the last paragraph is healed,
 * since a mark never spans a blank line; remend appends its closers at the very end of the text, so
 * a mark left open in an earlier paragraph got its closer in the last one, or in an open code block
 * (`**a\n\nb` healed to `**a\n\nb**`); closers go before trailing whitespace, where they can close
 * (remend healed `**a\n` to `**a\n**`); a link's URL is whole only once its parentheses balance; and
 * the marks around and inside a link still on its way are healed too ({@link healLink}).
 */

/** Where a link whose URL has not arrived points; the renderer draws its label as a link to come. */
export const INCOMPLETE_LINK = "streamdown:incomplete-link";
/** Where an image whose URL has not arrived points. */
export const INCOMPLETE_IMAGE = "streamdown:incomplete-image";

// ---- scan: one pass classifies every position as prose or code
//
// Fence and span semantics follow CommonMark:
//
// - A fence opens only at the start of a line, with any indentation, after any block quote and list
//   markers. Reading an indented line as code is the safe direction: healing then leaves it alone.
// - A fence inside a block quote reads its lines after the quote markers and ends with the quote; a
//   fence opened on a list marker's line ends with the item, at a non-blank line indented short of
//   its content.
// - Both ``` and ~~~ fences, runs of 3 or more; a backtick fence's info string has no backtick.
// - A fence closes on a run of the same character at least as long as the opener, alone on its line.
// - An inline code span opened by N backticks closes only on a run of exactly N, and cannot cross a
//   blank line.

const PROSE = 0;
/** The ``` or ~~~ run that opens or closes a fence */
const FENCE_MARKER = 1;
/** The info string on a fence opener line */
const FENCE_INFO = 2;
const FENCE_BODY = 3;
/** A complete inline code span, with its backticks */
const CODE_SPAN = 4;
/** An inline code span whose closing run has not arrived */
const CODE_SPAN_OPEN = 5;

interface OpenFence {
  char: "`" | "~";
  /** Length of the opening run; a closer must be at least this long */
  length: number;
  /** Content column of the list item the opener line starts; 0 when it starts none */
  listIndent: number;
  /** Block quote markers before the opener; each body line must repeat them */
  quoteDepth: number;
}

interface OpenSpan {
  runLength: number;
  start: number;
}

interface TextScan {
  text: string;
  regions: Uint8Array;
  openFence: OpenFence | null;
  openSpan: OpenSpan | null;
  /** Positions inside a link or image URL; computed when first needed */
  linkUrlMask: Uint8Array | null;
}

const FENCE_RUN = /^(`{3,}|~{3,})(.*)$/;

const isDigit = (char: string | undefined) =>
  char !== undefined && char >= "0" && char <= "9";

/** Index just past a list marker and its space at `start`, or -1. */
function skipListMarker(text: string, start: number) {
  let i = start;
  if (text[i] === "-" || text[i] === "*" || text[i] === "+") i += 1;
  else {
    while (isDigit(text[i]) && i - start < 9) i += 1;
    if (i === start || (text[i] !== "." && text[i] !== ")")) return -1;
    i += 1;
  }
  return text[i] === " " ? i + 1 : -1;
}

/** The block quote and list markers that open a line, in one forward pass. */
function skipContainerPrefix(text: string, lineStart: number, lineEnd: number) {
  let quoteDepth = 0;
  let afterQuotes = lineStart;
  let startsItem = false;
  let i = lineStart;
  for (;;) {
    while (i < lineEnd && text[i] === " ") i += 1;
    if (text[i] === ">") {
      quoteDepth += 1;
      i += text[i + 1] === " " ? 2 : 1;
      afterQuotes = i;
      startsItem = false;
      continue;
    }
    const afterMarker = i < lineEnd ? skipListMarker(text, i) : -1;
    if (afterMarker === -1)
      return {
        contentStart: i,
        listIndent: startsItem ? i - afterQuotes : 0,
        quoteDepth,
      };
    i = afterMarker;
    startsItem = true;
  }
}

/** Index just past up to `depth` quote markers at a line's start, or -1 if it has fewer. */
function skipQuoteMarkers(
  text: string,
  lineStart: number,
  lineEnd: number,
  depth: number,
) {
  let i = lineStart;
  for (let found = 0; found < depth; found += 1) {
    while (i < lineEnd && text[i] === " ") i += 1;
    if (text[i] !== ">") return -1;
    i += 1;
    if (text[i] === " ") i += 1;
  }
  return i;
}

function isFenceCloser(
  text: string,
  contentStart: number,
  lineEnd: number,
  fence: OpenFence,
) {
  let i = contentStart;
  while (i < lineEnd && text[i] === " ") i += 1;
  let run = 0;
  while (i < lineEnd && text[i] === fence.char) {
    i += 1;
    run += 1;
  }
  if (run < fence.length) return false;
  for (; i < lineEnd; i += 1)
    if (text[i] !== " " && text[i] !== "\t" && text[i] !== "\r") return false;
  return true;
}

function openFenceAt(
  text: string,
  regions: Uint8Array,
  lineStart: number,
  lineEnd: number,
): OpenFence | null {
  const contentEnd =
    lineEnd > lineStart && text[lineEnd - 1] === "\r" ? lineEnd - 1 : lineEnd;
  const prefix = skipContainerPrefix(text, lineStart, contentEnd);
  const opener = text.slice(prefix.contentStart, contentEnd).match(FENCE_RUN);
  if (!opener) return null;
  const [, run, info] = opener;
  const char = run[0] as "`" | "~";
  // a backtick fence's info string cannot hold a backtick; such a line is inline code
  if (char === "`" && info.includes("`")) return null;
  const markerStart = prefix.contentStart;
  regions.fill(FENCE_MARKER, markerStart, markerStart + run.length);
  regions.fill(
    FENCE_INFO,
    markerStart + run.length,
    Math.min(lineEnd + 1, regions.length),
  );
  return {
    char,
    length: run.length,
    listIndent: prefix.listIndent,
    quoteDepth: prefix.quoteDepth,
  };
}

/** Where a body line's content starts inside its fence's containers, or -1 if it leaves them. */
function fenceContentStart(
  text: string,
  lineStart: number,
  lineEnd: number,
  fence: OpenFence,
) {
  const contentStart = skipQuoteMarkers(
    text,
    lineStart,
    lineEnd,
    fence.quoteDepth,
  );
  if (contentStart === -1 || fence.listIndent === 0) return contentStart;
  let i = contentStart;
  while (i < lineEnd && text[i] === " ") i += 1;
  const blank = i === lineEnd || (text[i] === "\r" && i + 1 === lineEnd);
  return blank || i - contentStart >= fence.listIndent ? contentStart : -1;
}

function paintFences(text: string, regions: Uint8Array) {
  const n = text.length;
  let fence: OpenFence | null = null;
  for (let lineStart = 0; lineStart < n;) {
    let lineEnd = text.indexOf("\n", lineStart);
    if (lineEnd === -1) lineEnd = n;
    const contentStart = fence
      ? fenceContentStart(text, lineStart, lineEnd, fence)
      : -1;
    // the block quote or list item ended, taking the fence with it
    if (fence && contentStart === -1) fence = null;
    if (!fence) fence = openFenceAt(text, regions, lineStart, lineEnd);
    else if (isFenceCloser(text, contentStart, lineEnd, fence)) {
      regions.fill(FENCE_MARKER, lineStart, lineEnd);
      fence = null;
    } else regions.fill(FENCE_BODY, lineStart, Math.min(lineEnd + 1, n));
    lineStart = lineEnd + 1;
  }
  return fence;
}

function backtickRunEnd(text: string, start: number) {
  let end = start + 1;
  while (end < text.length && text[end] === "`") end += 1;
  return end;
}

/** A blank line ends the paragraph, and with it any chance of closing a span. */
function isParagraphBreakAt(text: string, newline: number) {
  let j = newline + 1;
  while (
    j < text.length &&
    (text[j] === " " || text[j] === "\t" || text[j] === "\r")
  )
    j += 1;
  return j < text.length && text[j] === "\n";
}

/** The next prose backtick that could open a span, or -1. */
function nextSpanOpener(text: string, regions: Uint8Array, from: number) {
  for (let i = from; ;) {
    const next = text.indexOf("`", i);
    if (next === -1) return -1;
    const escaped =
      next > i && text[next - 1] === "\\" && regions[next - 1] === PROSE;
    if (regions[next] === PROSE && !escaped) return next;
    i = next + 1;
  }
}

function paintSpans(text: string, regions: Uint8Array): OpenSpan | null {
  const n = text.length;
  let spanStart = -1;
  let spanRun = 0;
  for (let i = 0; i < n;) {
    if (spanStart < 0) {
      i = nextSpanOpener(text, regions, i);
      if (i === -1) break;
      const end = backtickRunEnd(text, i);
      spanStart = i;
      spanRun = end - i;
      i = end;
      continue;
    }
    // inside a span: backslashes are literal in code
    if (regions[i] !== PROSE) {
      // a span cannot cross into a fence
      regions.fill(CODE_SPAN_OPEN, spanStart, i);
      spanStart = -1;
      i += 1;
      continue;
    }
    if (text[i] === "\n" && isParagraphBreakAt(text, i)) {
      // the unmatched opener stays literal in its finished paragraph
      spanStart = -1;
      i += 1;
      continue;
    }
    if (text[i] !== "`") {
      i += 1;
      continue;
    }
    const end = backtickRunEnd(text, i);
    // a run of another length is literal inside the span
    if (end - i === spanRun) {
      regions.fill(CODE_SPAN, spanStart, end);
      spanStart = -1;
    }
    i = end;
  }
  if (spanStart < 0) return null;
  regions.fill(CODE_SPAN_OPEN, spanStart, n);
  return { start: spanStart, runLength: spanRun };
}

// The last scan is memoized: handlers query many positions of one string, and each handler hands
// its output to the next, so one entry keeps queries O(1) and each handler O(n).
let cached: TextScan | null = null;

function getScan(text: string): TextScan {
  if (cached?.text === text) return cached;
  const regions = new Uint8Array(text.length);
  const openFence = paintFences(text, regions);
  const openSpan = paintSpans(text, regions);
  cached = { text, regions, openFence, openSpan, linkUrlMask: null };
  return cached;
}

/** Whether `position` is in a fence or code span (past the end: whether one is still open). */
function isCodeAt(scan: TextScan, position: number) {
  if (position >= scan.regions.length)
    return scan.openFence !== null || scan.openSpan !== null;
  return position >= 0 && scan.regions[position] !== PROSE;
}

function isFenceAt(scan: TextScan, position: number) {
  if (position >= scan.regions.length) return scan.openFence !== null;
  if (position < 0) return false;
  const region = scan.regions[position];
  return (
    region === FENCE_MARKER || region === FENCE_INFO || region === FENCE_BODY
  );
}

const isInsideCode = (text: string, position: number) =>
  isCodeAt(getScan(text), position);
const isWithinCompleteInlineCode = (text: string, position: number) =>
  getScan(text).regions[position] === CODE_SPAN;

/** Whether `position` is inside a fenced code block. */
export const isWithinCodeBlock = (text: string, position: number) =>
  isFenceAt(getScan(text), position);

/** Counts non-overlapping pairs of `char` (`**`, `~~`) in prose. */
function countDoublePairs(text: string, char: string) {
  const { regions } = getScan(text);
  const pair = char + char;
  let count = 0;
  for (let i = text.indexOf(pair); i !== -1; i = text.indexOf(pair, i)) {
    if (regions[i] === PROSE) {
      count += 1;
      i += 2;
    } else i += 1;
  }
  return count;
}

/** One line's URL positions: those between a `](` opener and the next `)` on the line. */
function paintLinkUrlLine(
  scan: TextScan,
  lineStart: number,
  lineEnd: number,
  mask: Uint8Array,
) {
  const { text, regions } = scan;
  const closerFollows = new Uint8Array(lineEnd - lineStart);
  let seen = 0;
  for (let i = lineEnd - 1; i >= lineStart; i -= 1) {
    if (text[i] === ")" && regions[i] === PROSE) seen = 1;
    closerFollows[i - lineStart] = seen;
  }
  let inUrl = false;
  for (let i = lineStart; i < lineEnd; i += 1) {
    if (inUrl && closerFollows[i - lineStart] === 1) mask[i] = 1;
    if (regions[i] !== PROSE) continue;
    if (text[i] === ")") inUrl = false;
    else if (text[i] === "(") inUrl = i > 0 && text[i - 1] === "]";
  }
}

const EMPTY_MASK = new Uint8Array(0);

function inLinkUrlAt(scan: TextScan, position: number) {
  if (position < 0 || position >= scan.text.length) return false;
  if (scan.linkUrlMask === null) {
    const { text } = scan;
    if (!text.includes("](")) scan.linkUrlMask = EMPTY_MASK;
    else {
      const mask = new Uint8Array(text.length);
      for (let lineStart = 0; lineStart < text.length;) {
        let lineEnd = text.indexOf("\n", lineStart);
        if (lineEnd === -1) lineEnd = text.length;
        paintLinkUrlLine(scan, lineStart, lineEnd, mask);
        lineStart = lineEnd + 1;
      }
      scan.linkUrlMask = mask;
    }
  }
  return scan.linkUrlMask[position] === 1;
}

// ---- shared helpers

const LETTER_NUMBER_UNDERSCORE = /[\p{L}\p{N}_]/u;
const WHITESPACE_OR_MARKERS = /^[\s_~*`]*$/;
const LIST_ITEM = /^[\s]*[-*+][\s]+$/;
const FOUR_OR_MORE_ASTERISKS = /^\*{4,}$/;

/** A letter, number or underscore, with an ASCII fast path. */
export function isWordChar(char: string) {
  if (!char) return false;
  const code = char.charCodeAt(0);
  if (
    (code >= 48 && code <= 57) ||
    (code >= 65 && code <= 90) ||
    (code >= 97 && code <= 122) ||
    code === 95
  )
    return true;
  return LETTER_NUMBER_UNDERSCORE.test(char);
}

const isWhitespaceChar = (char: string) =>
  char === " " || char === "\t" || char === "\n";

function findMatchingOpeningBracket(text: string, closeIndex: number) {
  let depth = 1;
  for (let i = closeIndex - 1; i >= 0; i -= 1) {
    if (text[i] === "]") depth += 1;
    else if (text[i] === "[" && --depth === 0) return i;
  }
  return -1;
}

function findMatchingClosingBracket(text: string, openIndex: number) {
  let depth = 1;
  for (let i = openIndex + 1; i < text.length; i += 1) {
    if (text[i] === "[") depth += 1;
    else if (text[i] === "]" && --depth === 0) return i;
  }
  return -1;
}

/** Whether the marker's line is a thematic break: 3 or more markers and only whitespace besides. */
function isHorizontalRule(text: string, markerIndex: number, marker: string) {
  const lineStart = text.lastIndexOf("\n", markerIndex - 1) + 1;
  let lineEnd = text.indexOf("\n", markerIndex);
  if (lineEnd === -1) lineEnd = text.length;
  let markers = 0;
  for (const char of text.substring(lineStart, lineEnd)) {
    if (char === marker) markers += 1;
    else if (char !== " " && char !== "\t") return false;
  }
  return markers >= 3;
}

/**
 * An end-anchored pattern whose delimiter may follow the opening marker only as the text's final
 * character. Matching starts at the last delimiter before the final character (or the final
 * character), so the rest of the document is never scanned.
 */
interface TrailingPattern {
  delimiter: string;
  markerLength: number;
  regex: RegExp;
}

function matchTrailing(
  text: string,
  { delimiter, markerLength, regex }: TrailingPattern,
) {
  const last = text.lastIndexOf(delimiter, text.length - 2);
  const markerEnd = last === -1 ? text.length - 1 : last;
  return text.slice(Math.max(0, markerEnd - markerLength + 1)).match(regex);
}

const BOLD = { delimiter: "*", markerLength: 2, regex: /(\*\*)([^*]*\*?)$/ };
const DOUBLE_UNDERSCORE = {
  delimiter: "_",
  markerLength: 2,
  regex: /(__)([^_]*?)$/,
};
const BOLD_ITALIC = {
  delimiter: "*",
  markerLength: 3,
  regex: /(\*\*\*)([^*]*?)$/,
};
const SINGLE_ASTERISK = {
  delimiter: "*",
  markerLength: 1,
  regex: /(\*)([^*]*?)$/,
};
const SINGLE_UNDERSCORE = {
  delimiter: "_",
  markerLength: 1,
  regex: /(_)([^_]*?)$/,
};
const STRIKETHROUGH = {
  delimiter: "~",
  markerLength: 2,
  regex: /(~~)([^~]*?)$/,
};
const HALF_COMPLETE_UNDERSCORE = {
  delimiter: "_",
  markerLength: 2,
  regex: /(__)([^_]+)_$/,
};
const HALF_COMPLETE_TILDE = {
  delimiter: "~",
  markerLength: 2,
  regex: /(~~)([^~]+)~$/,
};

/**
 * Whether a marker should stay open: nothing meaningful follows it, it opens a list item whose
 * content runs on to another line, or its line is a thematic break.
 */
function shouldSkipCompletion(
  text: string,
  content: string,
  markerIndex: number,
  marker: string,
) {
  if (!content || WHITESPACE_OR_MARKERS.test(content)) return true;
  const lineStart = text.lastIndexOf("\n", markerIndex - 1) + 1;
  if (
    LIST_ITEM.test(text.substring(lineStart, markerIndex)) &&
    content.includes("\n")
  )
    return true;
  return isHorizontalRule(text, markerIndex, marker);
}

// ---- comparison operators: `- > 25` in a list item is "greater than", not a block quote

const LIST_COMPARISON = /^(\s*(?:[-*+]|\d+[.)]) +)>(=?\s*[$]?\d)/gm;

function handleComparisonOperators(text: string) {
  if (!text.includes(">")) return text;
  return text.replace(LIST_COMPARISON, (match, prefix, suffix, offset) =>
    isInsideCode(text, offset) ? match : `${prefix}\\>${suffix}`,
  );
}

// ---- links and images

function handleIncompleteUrl(text: string, lastParen: number) {
  // the URL is whole once its parentheses balance: `(https://a.com/x_(y)` is not yet
  let depth = 1;
  for (let i = lastParen + 2; i < text.length && depth > 0; i += 1)
    if (text[i] === "(") depth += 1;
    else if (text[i] === ")") depth -= 1;
  if (depth === 0) return null;
  const open = findMatchingOpeningBracket(text, lastParen);
  if (open === -1 || isInsideCode(text, open)) return null;
  const isImage = open > 0 && text[open - 1] === "!";
  const before = text.substring(0, isImage ? open - 1 : open);
  const label = text.substring(open + 1, lastParen);
  return isImage
    ? `${before}![${label}](${INCOMPLETE_IMAGE})`
    : `${before}[${label}](${INCOMPLETE_LINK})`;
}

/** The `[` at `i` has no closing `]` yet: close it as a link or image to come. */
function handleIncompleteText(text: string, i: number) {
  const isImage = i > 0 && text[i - 1] === "!";
  if (
    text.substring(i + 1).includes("]") &&
    findMatchingClosingBracket(text, i) !== -1
  )
    return null;
  if (isImage)
    return `${text.substring(0, i - 1)}![${text.substring(i + 1)}](${INCOMPLETE_IMAGE})`;
  return `${text}](${INCOMPLETE_LINK})`;
}

function healTrailingLinkOrImage(text: string) {
  const lastParen = text.lastIndexOf("](");
  if (lastParen !== -1 && !isInsideCode(text, lastParen)) {
    const result = handleIncompleteUrl(text, lastParen);
    if (result !== null) return result;
  }
  for (
    let i = text.lastIndexOf("[");
    i !== -1;
    i = i === 0 ? -1 : text.lastIndexOf("[", i - 1)
  ) {
    if (isInsideCode(text, i)) continue;
    const result = handleIncompleteText(text, i);
    if (result !== null) return result;
  }
  return text;
}

/** A bound on healing passes, so an adversarial tail of nested constructs stays linear. */
const MAX_HEAL_PASSES = 32;

function handleIncompleteLinksAndImages(text: string) {
  let current = text;
  // removing a trailing image can expose another incomplete construct; heal until it holds
  for (let pass = 0; pass < MAX_HEAL_PASSES; pass += 1) {
    const next = healTrailingLinkOrImage(current);
    if (next.length >= current.length) return next;
    current = next;
  }
  return current;
}

// ---- emphasis

function shouldSkipAsterisk(
  scan: TextScan,
  index: number,
  prev: string,
  next: string,
) {
  if (prev === "\\") return true;
  // the first * of *** counts as a single asterisk: it can close a single * italic
  // (`**bold and *italic***`); the first of ** does not
  if (prev !== "*" && next === "*") return scan.text[index + 2] !== "*";
  if (prev === "*") return true;
  // flanked by whitespace on both sides: not a delimiter (and so not a list marker)
  return (!prev || isWhitespaceChar(prev)) && (!next || isWhitespaceChar(next));
}

const isWordInternal = (prev: string, next: string) =>
  Boolean(prev && next && isWordChar(prev) && isWordChar(next));

/**
 * Whether a single asterisk counts towards the open/close parity. Word-internal asterisks count only
 * while resolving an active run: they can close one and reopen in the same word after a close, but
 * a cold one (`hello*world`) stays literal, as does a right-flanking marker after a closed run.
 */
function countsSingleAsterisk(
  prev: string,
  next: string,
  count: number,
  inChain: boolean,
): { count: true; inChain: boolean } | { count: false } {
  const internal = isWordInternal(prev, next);
  const canOpen = Boolean(next) && !isWhitespaceChar(next);
  const canClose = Boolean(prev) && !isWhitespaceChar(prev);
  if (internal && count % 2 === 0 && !inChain) return { count: false };
  if ((canClose && count % 2 === 1) || canOpen)
    return { count: true, inChain: internal };
  return { count: false };
}

function countSingleAsterisks(text: string) {
  const scan = getScan(text);
  let count = 0;
  let inChain = false;
  for (let i = 0; i < text.length; i += 1) {
    if (text[i] !== "*" || scan.regions[i] !== PROSE) {
      if (!isWordChar(text[i])) inChain = false;
      continue;
    }
    const prev = i > 0 ? text[i - 1] : "";
    const next = i < text.length - 1 ? text[i + 1] : "";
    if (shouldSkipAsterisk(scan, i, prev, next)) continue;
    const decision = countsSingleAsterisk(prev, next, count, inChain);
    if (decision.count) {
      count += 1;
      inChain = decision.inChain;
    }
  }
  return count;
}

function shouldSkipUnderscore(
  scan: TextScan,
  index: number,
  prev: string,
  next: string,
) {
  if (prev === "\\" || inLinkUrlAt(scan, index)) return true;
  if (prev === "_" || next === "_") return true;
  return isWordInternal(prev, next);
}

function countSingleUnderscores(text: string) {
  const scan = getScan(text);
  let count = 0;
  for (let i = 0; i < text.length; i += 1) {
    if (text[i] !== "_" || scan.regions[i] !== PROSE) continue;
    const prev = i > 0 ? text[i - 1] : "";
    const next = i < text.length - 1 ? text[i + 1] : "";
    if (!shouldSkipUnderscore(scan, i, prev, next)) count += 1;
  }
  return count;
}

/** Triple asterisks outside code, not counting runs of four or more as more than their threes. */
function countTripleAsterisks(text: string) {
  const { regions } = getScan(text);
  let count = 0;
  let run = 0;
  for (let i = 0; i <= text.length; i += 1) {
    if (i < text.length && text[i] === "*" && regions[i] === PROSE) run += 1;
    else {
      if (run >= 3) count += Math.floor(run / 3);
      run = 0;
    }
  }
  return count;
}

const countDoubleAsterisks = (text: string) => countDoublePairs(text, "*");

const isLineBoundaryChar = (char: string) =>
  char === "" || char === " " || char === "\t" || char === "\n";

/**
 * Whether the text has an unmatched `__`, counted per maximal underscore run. A run gives
 * floor(length / 2) pairs and flips parity only when that is odd; a word-internal run
 * (`snake__case`) is an identifier; runs in code or a link URL are skipped; a run alone on its line
 * is a thematic break.
 */
function hasUnmatchedDoubleUnderscore(text: string) {
  const scan = getScan(text);
  const n = text.length;
  const memo = { lineEnd: -1, result: false };
  let unmatched = false;
  for (let i = 0; i < n;) {
    if (text[i] !== "_" || scan.regions[i] !== PROSE) {
      i += 1;
      continue;
    }
    let start = i;
    let end = i + 1;
    while (end < n && text[end] === "_" && scan.regions[end] === PROSE)
      end += 1;
    i = end;
    // a backslash escapes the run's first underscore; the rest still flanks as a delimiter
    let escaped = false;
    if (start > 0 && text[start - 1] === "\\") {
      start += 1;
      escaped = true;
    }
    if (end - start < 2) continue;
    const prev = escaped ? "\\" : start > 0 ? text[start - 1] : "";
    const next = end < n ? text[end] : "";
    if (isWordChar(prev) && isWordChar(next)) continue;
    if (isLineBoundaryChar(prev) && isLineBoundaryChar(next)) {
      if (start > memo.lineEnd) {
        const lineEnd = text.indexOf("\n", start);
        memo.lineEnd = lineEnd === -1 ? n : lineEnd;
        memo.result = isHorizontalRule(text, start, "_");
      }
      if (memo.result) continue;
    }
    if (inLinkUrlAt(scan, start)) continue;
    if (Math.floor((end - start) / 2) % 2 === 1) unmatched = !unmatched;
  }
  return unmatched;
}

const inCode = (text: string, index: number) =>
  isInsideCode(text, index) || isWithinCompleteInlineCode(text, index);

function handleIncompleteBold(text: string) {
  const match = matchTrailing(text, BOLD);
  if (!match) return text;
  const content = match[2];
  const markerIndex = text.lastIndexOf(match[1]);
  if (inCode(text, markerIndex)) return text;
  if (shouldSkipCompletion(text, content, markerIndex, "*")) return text;
  if (countDoubleAsterisks(text) % 2 === 1)
    // `**content*`: the trailing * is the first of the closing ** on its way
    return content.endsWith("*") ? `${text}*` : `${text}**`;
  return text;
}

function handleIncompleteDoubleUnderscoreItalic(text: string) {
  const match = matchTrailing(text, DOUBLE_UNDERSCORE);
  if (!match) {
    // `__content_`: the closing __ on its way
    const half = matchTrailing(text, HALF_COMPLETE_UNDERSCORE);
    if (
      half &&
      !inCode(text, text.lastIndexOf(half[1])) &&
      hasUnmatchedDoubleUnderscore(text)
    )
      return `${text}_`;
    return text;
  }
  const markerIndex = text.lastIndexOf(match[1]);
  if (inCode(text, markerIndex)) return text;
  if (shouldSkipCompletion(text, match[2], markerIndex, "_")) return text;
  return hasUnmatchedDoubleUnderscore(text) ? `${text}__` : text;
}

/** A lone, unescaped asterisk in prose: the only kind that can open an incomplete italic. */
function findFirstSingleAsteriskIndex(text: string) {
  const { regions } = getScan(text);
  for (let i = text.indexOf("*"); i !== -1; i = text.indexOf("*", i + 1)) {
    if (
      regions[i] !== PROSE ||
      text[i - 1] === "*" ||
      text[i + 1] === "*" ||
      text[i - 1] === "\\"
    )
      continue;
    const prev = i > 0 ? text[i - 1] : "";
    const next = i < text.length - 1 ? text[i + 1] : "";
    const nextIsWs = !next || isWhitespaceChar(next);
    // flanked by whitespace, word-internal and cold, or right-flanking only: not an opener
    if ((!prev || isWhitespaceChar(prev)) && nextIsWs) continue;
    if (isWordInternal(prev, next)) continue;
    if (nextIsWs) continue;
    return i;
  }
  return -1;
}

function handleIncompleteSingleAsteriskItalic(text: string) {
  if (!matchTrailing(text, SINGLE_ASTERISK)) return text;
  const first = findFirstSingleAsteriskIndex(text);
  if (first === -1 || inCode(text, first)) return text;
  const content = text.substring(first + 1);
  if (!content || WHITESPACE_OR_MARKERS.test(content)) return text;
  return countSingleAsterisks(text) % 2 === 1 ? `${text}*` : text;
}

function findFirstSingleUnderscoreIndex(text: string) {
  const scan = getScan(text);
  for (let i = text.indexOf("_"); i !== -1; i = text.indexOf("_", i + 1)) {
    if (
      scan.regions[i] === PROSE &&
      text[i - 1] !== "_" &&
      text[i + 1] !== "_" &&
      text[i - 1] !== "\\" &&
      !inLinkUrlAt(scan, i) &&
      !isWordInternal(i > 0 ? text[i - 1] : "", text[i + 1] ?? "")
    )
      return i;
  }
  return -1;
}

/** Closes a single underscore italic before any trailing newlines. */
function insertClosingUnderscore(text: string) {
  let end = text.length;
  while (end > 0 && text[end - 1] === "\n") end -= 1;
  return `${text.slice(0, end)}_${text.slice(end)}`;
}

/** `**` added to close a bold that opened before the `_`: the `_` closes first. */
function handleTrailingAsterisksForUnderscore(text: string) {
  if (!text.endsWith("**")) return null;
  const without = text.slice(0, -2);
  if (countDoubleAsterisks(without) % 2 !== 1) return null;
  const bold = without.indexOf("**");
  const underscore = findFirstSingleUnderscoreIndex(without);
  return bold !== -1 && underscore !== -1 && bold < underscore
    ? `${without}_**`
    : null;
}

function handleIncompleteSingleUnderscoreItalic(text: string) {
  if (!matchTrailing(text, SINGLE_UNDERSCORE)) return text;
  const first = findFirstSingleUnderscoreIndex(text);
  if (first === -1) return text;
  const content = text.substring(first + 1);
  if (!content || WHITESPACE_OR_MARKERS.test(content)) return text;
  if (inCode(text, first)) return text;
  if (countSingleUnderscores(text) % 2 === 1)
    return (
      handleTrailingAsterisksForUnderscore(text) ??
      insertClosingUnderscore(text)
    );
  return text;
}

function handleIncompleteBoldItalic(text: string) {
  if (FOUR_OR_MORE_ASTERISKS.test(text)) return text;
  const match = matchTrailing(text, BOLD_ITALIC);
  if (!match) return text;
  const markerIndex = text.lastIndexOf(match[1]);
  if (!match[2] || WHITESPACE_OR_MARKERS.test(match[2])) return text;
  if (inCode(text, markerIndex)) return text;
  if (isHorizontalRule(text, markerIndex, "*")) return text;
  if (countTripleAsterisks(text) % 2 === 1) {
    // with ** and * both balanced, the *** is overlapping markers (`**bold and *italic***`)
    if (
      countDoubleAsterisks(text) % 2 === 0 &&
      countSingleAsterisks(text) % 2 === 0
    )
      return text;
    return `${text}***`;
  }
  return text;
}

// ---- inline code and strikethrough

/**
 * Closes an open code span with what remains of its closing run: a span opened by N backticks
 * closes only on exactly N, and a trailing run of k < N is that closer on its way.
 */
function handleIncompleteInlineCode(text: string) {
  const scan = getScan(text);
  // backticks in a streaming fence are code; the renderer shows the fence as streaming code
  if (scan.openFence) return text;
  const span = scan.openSpan;
  if (!span) return text;
  const content = text.slice(span.start + span.runLength);
  if (!content || WHITESPACE_OR_MARKERS.test(content)) return text;
  let trailing = 0;
  for (let i = text.length - 1; i >= 0 && text[i] === "`"; i -= 1)
    trailing += 1;
  // a trailing run as long as the opener is literal in the span; more would only extend it
  if (trailing >= span.runLength) return text;
  return text + "`".repeat(span.runLength - trailing);
}

function handleIncompleteStrikethrough(text: string) {
  const match = matchTrailing(text, STRIKETHROUGH);
  if (match) {
    if (!match[2] || WHITESPACE_OR_MARKERS.test(match[2])) return text;
    if (inCode(text, text.lastIndexOf(match[1]))) return text;
    return countDoublePairs(text, "~") % 2 === 1 ? `${text}~~` : text;
  }
  // `~~content~`: the closing ~~ on its way
  const half = matchTrailing(text, HALF_COMPLETE_TILDE);
  if (!half || inCode(text, text.lastIndexOf(half[0].slice(0, 2)))) return text;
  return countDoublePairs(text, "~") % 2 === 1 ? `${text}~` : text;
}

/** Remend's handlers for the marks a paragraph leaves open, in its priority order. */
const MARK_HANDLERS: ((text: string) => string)[] = [
  handleIncompleteBoldItalic,
  handleIncompleteBold,
  handleIncompleteDoubleUnderscoreItalic,
  handleIncompleteSingleAsteriskItalic,
  handleIncompleteSingleUnderscoreItalic,
  handleIncompleteInlineCode,
  handleIncompleteStrikethrough,
];

/** Strips one trailing space, but not a hard break's two. */
const trimSpace = (text: string) =>
  text.endsWith(" ") && !text.endsWith("  ") ? text.slice(0, -1) : text;

/**
 * Heals Markdown that is still streaming: what it leaves open at its end is closed, so it renders as
 * it will once whole. Healing healed text changes nothing.
 */
export function heal(text: string): string {
  if (!text) return text;
  try {
    const whole = handleComparisonOperators(trimSpace(text));
    // only the last paragraph can be open: a mark never spans a blank line, so remend's closers
    // for a mark left open in an earlier paragraph would land in the wrong one (or in code)
    const start = lastParagraphStart(whole);
    // closers go before trailing whitespace, where they can close (`**a\n` → `**a**\n`)
    const space = /\s*$/.exec(whole.slice(start))![0];
    const result = healLink(
      handleIncompleteLinksAndImages(
        whole.slice(start, whole.length - space.length),
      ),
    );
    return trimSpace(whole.slice(0, start) + result + space);
  } finally {
    // a long reply's scan is not kept between calls
    cached = null;
  }
}

const healMarks = (text: string) =>
  MARK_HANDLERS.reduce((result, handle) => handle(result), text);

/** Stands in for a link still on its way while the marks around it are healed. */
const LINK_STANDIN = "\uE000";

/**
 * Heals the marks of a paragraph that may end in a link or image still on its way. Remend stops
 * there, leaving open the marks around the link (`*[链接](https://ex` kept its `*` literal); here the
 * link's label is healed on its own and the text around it with the link as one character, so each
 * closer lands on its side of the link.
 */
function healLink(text: string) {
  const at = Math.max(
    text.endsWith(`](${INCOMPLETE_LINK})`)
      ? text.lastIndexOf(`](${INCOMPLETE_LINK})`)
      : -1,
    text.endsWith(`](${INCOMPLETE_IMAGE})`)
      ? text.lastIndexOf(`](${INCOMPLETE_IMAGE})`)
      : -1,
  );
  const open = at < 0 ? -1 : findMatchingOpeningBracket(text, at);
  if (open < 0) return healMarks(text);
  const start = open > 0 && text[open - 1] === "!" ? open - 1 : open;
  const link = `${text.slice(start, open + 1)}${healMarks(text.slice(open + 1, at))}${text.slice(at)}`;
  const around = healMarks(text.slice(0, start) + LINK_STANDIN);
  const standin = around.lastIndexOf(LINK_STANDIN);
  return around.slice(0, standin) + link + around.slice(standin + 1);
}

/** Where the text's last paragraph starts: just past the last blank line in prose before it, or 0. */
function lastParagraphStart(text: string) {
  const { regions } = getScan(text);
  for (
    let i = text.lastIndexOf("\n");
    i >= 0;
    i = i ? text.lastIndexOf("\n", i - 1) : -1
  ) {
    if (regions[i] !== PROSE || !isParagraphBreakAt(text, i)) continue;
    const start = text.indexOf("\n", i + 1) + 1;
    // blank lines that end the text end no paragraph yet
    if (text.slice(start).trim()) return start;
  }
  return 0;
}
