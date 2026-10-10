/**
 * The text a reply shows: healed always, as Streamdown heals in its streaming mode, so a reply looks
 * the same the moment it ends as the moment before; and, while it streams, with its tail settled so
 * that what is shown is never taken back as more arrives.
 *
 * Healing ({@link heal}, ported from remend) closes what is open inline. The block-level waits here
 * are this package's own, for the last line, whose block is not known until more of it arrives.
 */
import { cells, DELIMITER, FENCE, RULE } from "./blocks";
import { heal } from "./heal";

/** A last line that is only a block's marker: `#`, `>`, `-`, `1.`, `` `` ``, `- [x`. */
const MARKER_ONLY =
  /^[ \t]*(?:#{1,6}|>|`{1,2}|~{1,2}|\d{1,9}[.)]?|[-*+_](?:[ \t]*[-*_])?|(?:[-*+]|\d{1,9}[.)])[ \t]+\[(?:[ xX]\]?)?)[ \t]*$/;
const TABLE_ROW = /^[ \t]*\|/;
const PARTIAL_DELIMITER = /^[ \t]*\|[ \t|:-]*$/;
const PARTIAL_FENCE_CLOSE = /^[ \t]*(?:`+|~+)$/;
const FENCE_CLOSE = /^[ \t]*(`{3,}|~{3,})[ \t]*$/;
const MARK_RUN = /(?:\*+|_+|~+|`+)$/;
const BRACKETED = /\[[^\]\n]*\]$/;

/** What `text` shows: healed, and while `streaming`, its tail settled. */
export function shown(text: string, streaming: boolean) {
  return heal(streaming ? settle(text) : text);
}

/**
 * The tail of a streaming text, settled: lines whose block is not known yet wait, and a mark run at
 * the very end waits to see whether it opens, closes or is text.
 */
function settle(text: string) {
  const lines = text.split("\n");
  let last = lines.length - 1;
  // an open fence: its line until its language is whole, and a closing fence's first backticks
  let fence = -1;
  for (let i = 0; i < lines.length; i += 1) {
    const marks = FENCE.exec(lines[i]);
    if (fence < 0) {
      if (marks) fence = i;
    } else {
      const close = FENCE_CLOSE.exec(lines[i]);
      if (close && close[1][0] === lines[fence].trimStart()[0]) fence = -1;
    }
  }
  if (fence >= 0) {
    if (fence === last || PARTIAL_FENCE_CLOSE.test(lines[last])) lines.pop();
    return lines.join("\n");
  }
  // a line just ended draws nothing yet; what it ended is still the tail
  if (last > 0 && !lines[last]) {
    lines.pop();
    last -= 1;
  }
  const line = lines[last];
  const above = lines[last - 1] ?? "";
  if (MARKER_ONLY.test(line)) lines.pop();
  // a table's header row waits for its delimiter row
  else if (TABLE_ROW.test(line) && !above.includes("|")) lines.pop();
  else if (
    PARTIAL_DELIMITER.test(line) &&
    !DELIMITER.test(line) &&
    TABLE_ROW.test(above) &&
    !(lines[last - 2] ?? "").includes("|")
  ) {
    // a delimiter row on its way already makes the table, with the columns its header has
    const typed = cells(line);
    lines[last] = `|${cells(above)
      .map((_, i) => (/^:?-+:?$/.test(typed[i] ?? "") ? typed[i] : "---"))
      .join("|")}|`;
  } else if (!RULE.test(line)) {
    const tail = lines.join("\n");
    // `[label]` at the very end is a link whose `(` is next: healing shows it as one to come
    return holdMarkRun(BRACKETED.test(tail) ? `${tail}(` : tail);
  }
  return lines.join("\n");
}

/**
 * A run of `*`, `_`, `~` or backticks at the very end that closes nothing yet could still open a
 * mark or be text: it waits. One that closes a mark stays, for healing to complete.
 */
function holdMarkRun(text: string) {
  const run = MARK_RUN.exec(text);
  if (!run) return text;
  const before = text.slice(0, run.index);
  return heal(before) === trimSpace(before) ? before : text;
}

/** Healing strips one trailing space (not a hard break's two); so does the comparison. */
const trimSpace = (text: string) =>
  text.endsWith(" ") && !text.endsWith("  ") ? text.slice(0, -1) : text;
