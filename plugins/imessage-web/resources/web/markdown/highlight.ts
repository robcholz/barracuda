/**
 * Syntax colouring for the code blocks in a reply, small enough to ship from the device: one
 * scanner over a grammar of comment markers, quotes and word lists, no regular-expression grammar
 * per language. It marks comments, strings, numbers and literals, keywords, called functions (and
 * Rust macros), capitalised type names and shell variables; everything else stays plain text.
 */

interface Grammar {
  /** Line comment openers. A `#` opens one only at the start of a word. */
  line: string[];
  /** Block comment delimiters. */
  block: [string, string][];
  /** Quote characters that open a string. */
  quotes: string;
  /** Python's `"""` and `'''`, tried before a single quote. */
  triple?: boolean;
  /** A `'` opens a string only as a character literal (`'a'`, `'\n'`): Rust, whose lifetimes read `'a`. */
  chars?: boolean;
  keywords: Set<string>;
  /** Words drawn like numbers: `true`, `null`, `None`. */
  literals: Set<string>;
  /** Keywords match whatever their case (SQL). */
  fold?: boolean;
  /** `$name` and `${…}` are variables (shell). */
  variables?: boolean;
  /** `#include`, `#define`: a `#` and the word after it, at a line's start, are a keyword (C). */
  directives?: boolean;
  /** A capitalised word is a type. */
  types?: boolean;
}

const words = (list: string) => new Set(list.split(" "));

const C_COMMENTS = {
  line: ["//"],
  block: [["/*", "*/"]] as [string, string][],
};
const HASH_COMMENTS = { line: ["#"], block: [] };

const RUST: Grammar = {
  ...C_COMMENTS,
  quotes: '"',
  chars: true,
  types: true,
  keywords: words(
    "as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while yield",
  ),
  literals: words("true false"),
};

const C: Grammar = {
  ...C_COMMENTS,
  quotes: "\"'",
  directives: true,
  types: true,
  keywords: words(
    "auto bool break case catch char class const constexpr continue default delete do double else enum explicit extern float for friend goto if inline int long namespace new noexcept operator override private protected public register restrict return short signed sizeof static static_assert struct switch template this throw try typedef typename union unsigned using virtual void volatile while uint8_t uint16_t uint32_t uint64_t int8_t int16_t int32_t int64_t size_t",
  ),
  literals: words("true false NULL nullptr"),
};

const JS: Grammar = {
  ...C_COMMENTS,
  quotes: "\"'`",
  types: true,
  keywords: words(
    "abstract as async await break case catch class const continue debugger declare default delete do else enum export extends finally for from function get if implements import in instanceof interface keyof let new of private protected public readonly return satisfies set static super switch this throw try type typeof var void while with yield",
  ),
  literals: words("true false null undefined NaN Infinity"),
};

const JSON_: Grammar = {
  line: [],
  block: [],
  quotes: '"',
  keywords: new Set(),
  literals: words("true false null"),
};

const PYTHON: Grammar = {
  ...HASH_COMMENTS,
  quotes: "\"'",
  triple: true,
  types: true,
  keywords: words(
    "and as assert async await break case class continue def del elif else except finally for from global if import in is lambda match nonlocal not or pass raise return self try while with yield",
  ),
  literals: words("True False None"),
};

const SHELL: Grammar = {
  ...HASH_COMMENTS,
  quotes: "\"'",
  variables: true,
  keywords: words(
    "case do done elif else esac exit export fi for function if in local readonly return select then until while",
  ),
  literals: words("true false"),
};

const GO: Grammar = {
  ...C_COMMENTS,
  quotes: "\"'`",
  types: true,
  keywords: words(
    "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var bool byte error float32 float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64",
  ),
  literals: words("true false nil iota"),
};

const JVM: Grammar = {
  ...C_COMMENTS,
  quotes: "\"'",
  types: true,
  keywords: words(
    "abstract as boolean break byte case catch char class companion const continue data default do double else enum extends extension final finally float for fun func guard if implements import in init int interface internal is let long namespace new object open override package private protected protocol public return sealed short static struct super switch this throw throws try typealias using val var void when where while",
  ),
  literals: words("true false null nil"),
};

const CONFIG: Grammar = {
  ...HASH_COMMENTS,
  quotes: "\"'",
  keywords: new Set(),
  literals: words("true false yes no on off null"),
};

const SQL: Grammar = {
  line: ["--"],
  block: [["/*", "*/"]],
  quotes: "'\"",
  fold: true,
  keywords: words(
    "add all alter and as asc begin between by case check column commit constraint create default delete desc distinct drop else end exists foreign from group having if in index inner insert integer into is join key left like limit not null offset on or order outer primary references right rollback select set table text then union unique update values varchar view when where with",
  ),
  literals: words("true false"),
};

const LUA: Grammar = {
  line: ["--"],
  block: [],
  quotes: "\"'",
  keywords: words(
    "and break do else elseif end for function goto if in local not or repeat return then until while",
  ),
  literals: words("true false nil"),
};

const GRAMMARS: Record<string, Grammar> = {
  rust: RUST,
  rs: RUST,
  c: C,
  h: C,
  cpp: C,
  "c++": C,
  cc: C,
  hpp: C,
  js: JS,
  javascript: JS,
  jsx: JS,
  mjs: JS,
  ts: JS,
  typescript: JS,
  tsx: JS,
  json: JSON_,
  jsonc: JS,
  json5: JS,
  py: PYTHON,
  python: PYTHON,
  sh: SHELL,
  bash: SHELL,
  shell: SHELL,
  zsh: SHELL,
  console: SHELL,
  go: GO,
  golang: GO,
  java: JVM,
  kotlin: JVM,
  kt: JVM,
  swift: JVM,
  cs: JVM,
  csharp: JVM,
  toml: CONFIG,
  yaml: CONFIG,
  yml: CONFIG,
  ini: CONFIG,
  conf: CONFIG,
  dockerfile: CONFIG,
  sql: SQL,
  lua: LUA,
};

const isWord = (c: string) => /[\w$]/.test(c);
const isDigit = (c: string) => c >= "0" && c <= "9";

/**
 * Colours `code` as `lang` (a fence's info word, any case): text and `span.bc-tok-*` nodes whose
 * text joins back to `code`, or null for a language it does not know.
 */
export function highlight(code: string, lang: string): Node[] | null {
  const name = lang.toLowerCase();
  if (name === "diff" || name === "patch") return diff(code);
  const grammar = GRAMMARS[name];
  return grammar ? scan(code, grammar) : null;
}

function emitter() {
  const nodes: Node[] = [];
  let plain = "";
  const flush = () => {
    if (plain) nodes.push(document.createTextNode(plain));
    plain = "";
  };
  return {
    nodes,
    text(value: string) {
      plain += value;
    },
    token(kind: string, value: string) {
      flush();
      const span = document.createElement("span");
      span.className = `bc-tok-${kind}`;
      span.textContent = value;
      nodes.push(span);
    },
    done() {
      flush();
      return nodes;
    },
  };
}

/** The index just past a string opened by `quote` at `from`; an unclosed one runs to the end. */
function stringEnd(code: string, from: number, quote: string) {
  let i = from + quote.length;
  while (i < code.length) {
    if (code[i] === "\\") i += 2;
    else if (code.startsWith(quote, i)) return i + quote.length;
    // a single-line quote ends at the line's end rather than colouring the rest of the block
    else if (code[i] === "\n" && quote !== "`" && quote.length === 1) return i;
    else i += 1;
  }
  return code.length;
}

function scan(code: string, g: Grammar): Node[] {
  const out = emitter();
  let i = 0;
  let lineStart = true;
  while (i < code.length) {
    const c = code[i];
    const prev = i ? code[i - 1] : "\n";
    if (c === "\n") {
      out.text(c);
      i += 1;
      lineStart = true;
      continue;
    }
    if (c === " " || c === "\t") {
      out.text(c);
      i += 1;
      continue;
    }
    const startOfLine = lineStart;
    lineStart = false;

    const opener = g.line.find(
      (marker) =>
        code.startsWith(marker, i) && (marker !== "#" || !isWord(prev)),
    );
    if (opener && !(g.directives && opener === "#" && startOfLine)) {
      const end = code.indexOf("\n", i);
      const stop = end < 0 ? code.length : end;
      out.token("c", code.slice(i, stop));
      i = stop;
      continue;
    }
    const block = g.block.find(([open]) => code.startsWith(open, i));
    if (block) {
      const end = code.indexOf(block[1], i + block[0].length);
      const stop = end < 0 ? code.length : end + block[1].length;
      out.token("c", code.slice(i, stop));
      i = stop;
      continue;
    }
    if (g.directives && c === "#" && startOfLine) {
      let j = i + 1;
      while (j < code.length && /[ \t]/.test(code[j])) j += 1;
      while (j < code.length && isWord(code[j])) j += 1;
      out.token("k", code.slice(i, j));
      i = j;
      continue;
    }
    if (g.quotes.includes(c) || (c === "'" && g.chars)) {
      if (c === "'" && g.chars) {
        const literal = /^'(?:\\[^']{1,8}|[^\\'\n])'/.exec(
          code.slice(i, i + 12),
        );
        if (!literal) {
          out.text(c);
          i += 1;
          continue;
        }
        out.token("s", literal[0]);
        i += literal[0].length;
        continue;
      }
      const triple = c.repeat(3);
      const quote = g.triple && code.startsWith(triple, i) ? triple : c;
      const end = stringEnd(code, i, quote);
      out.token("s", code.slice(i, end));
      i = end;
      continue;
    }
    if (g.variables && c === "$") {
      const match = /^\$(?:\{[^}\n]*\}?|\w+|[@#?$!*0-9-])/.exec(code.slice(i));
      if (match) {
        out.token("v", match[0]);
        i += match[0].length;
        continue;
      }
    }
    if (isDigit(c) && !isWord(prev)) {
      let j = i + 1;
      while (
        j < code.length &&
        /[\w.]/.test(code[j]) &&
        !(code[j] === "." && code[j + 1] === ".")
      )
        j += 1;
      out.token("n", code.slice(i, j));
      i = j;
      continue;
    }
    if (isWord(c) && !isDigit(c)) {
      let j = i + 1;
      while (j < code.length && isWord(code[j])) j += 1;
      const word = code.slice(i, j);
      const key = g.fold ? word.toLowerCase() : word;
      let after = j;
      while (code[after] === " ") after += 1;
      if (g.keywords.has(key)) out.token("k", word);
      else if (g.literals.has(key)) out.token("n", word);
      else if (
        g === RUST &&
        code[j] === "!" &&
        /[([{]/.test(code[j + 1] ?? "")
      ) {
        j += 1;
        out.token("f", code.slice(i, j));
      } else if (code[after] === "(") out.token("f", word);
      else if (g.types && /^[A-Z]/.test(word) && /[a-z]/.test(word))
        out.token("t", word);
      else out.text(word);
      i = j;
      continue;
    }
    out.text(c);
    i += 1;
  }
  return out.done();
}

/** A diff colours whole lines: additions, removals and hunk headers. */
function diff(code: string): Node[] {
  const out = emitter();
  for (const line of code.split(/(?<=\n)/)) {
    if (/^(\+\+\+|---)/.test(line)) out.token("k", line);
    else if (line.startsWith("+")) out.token("s", line);
    else if (line.startsWith("-")) out.token("d", line);
    else if (line.startsWith("@@")) out.token("f", line);
    else out.text(line);
  }
  return out.done();
}
