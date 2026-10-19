/** Line diffs for the Changes view: hunks of changed lines with three lines of context. */

export interface DiffLine {
  kind: " " | "+" | "-";
  /** The line's number before the change, for kept and removed lines. */
  old?: number;
  /** Its number after, for kept and added lines. */
  new?: number;
  text: string;
}

export type DiffRow = { hunk: string } | DiffLine;

/** Every line of `before` and `after`, kept, removed or added, in order. */
export function diffLines(before: string, after: string): DiffLine[] {
  const a = before === "" ? [] : before.split("\n");
  const b = after === "" ? [] : after.split("\n");
  const cols = b.length + 1;
  const table = new Uint32Array((a.length + 1) * cols);
  for (let i = a.length - 1; i >= 0; i -= 1)
    for (let j = b.length - 1; j >= 0; j -= 1)
      table[i * cols + j] =
        a[i] === b[j]
          ? table[(i + 1) * cols + j + 1] + 1
          : Math.max(table[(i + 1) * cols + j], table[i * cols + j + 1]);
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      out.push({ kind: " ", old: i + 1, new: j + 1, text: a[i] });
      i += 1;
      j += 1;
    } else if (
      i < a.length &&
      (j === b.length || table[(i + 1) * cols + j] >= table[i * cols + j + 1])
    ) {
      // Removed lines come before the lines that replace them.
      out.push({ kind: "-", old: i + 1, text: a[i] });
      i += 1;
    } else {
      out.push({ kind: "+", new: j + 1, text: b[j] });
      j += 1;
    }
  }
  return out;
}

/** The changed lines in hunks, each with `context` kept lines around it and an `@@` header. */
export function hunks(before: string, after: string, context = 3): DiffRow[] {
  const lines = diffLines(before, after);
  const keep = lines.map(() => false);
  lines.forEach((line, x) => {
    if (line.kind === " ") return;
    for (
      let k = Math.max(0, x - context);
      k <= Math.min(lines.length - 1, x + context);
      k += 1
    )
      keep[k] = true;
  });
  const rows: DiffRow[] = [];
  let open = false;
  lines.forEach((line, x) => {
    if (!keep[x]) {
      open = false;
      return;
    }
    if (!open) {
      rows.push({ hunk: `@@ ${line.old ?? line.new} @@` });
      open = true;
    }
    rows.push(line);
  });
  return rows;
}
