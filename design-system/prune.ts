/**
 * Cuts the design system down to what a page draws. The portal ships its stylesheet from device
 * flash, so its build keeps only the `bc-*` rules its sources use, the tokens those rules read,
 * and the keyframes and `@property` registrations they name.
 *
 * Sources are scanned as text for class names: `bc-name` is used where it appears, and a name
 * ending in `-` (`bc-badge--${tone}`, `"bc-tok-" + kind`) uses every class that starts with it.
 * A selector survives when every `bc-*` class it requires is used; classes inside `:not(…)` are
 * not required. Classes outside the `bc-` namespace are never cut.
 *
 * Rules a page brings itself (`provided`: the stylesheets it adds while open) are left to it:
 * each declaration a provided rule sets for the same selector, and each keyframes it defines, is
 * taken out.
 */

type Node =
  | { kind: "rule"; selector: string; body: string }
  | { kind: "block"; prelude: string; children: Node[] }
  | { kind: "raw"; prelude: string; body: string | null };

/** The class names, class-name prefixes and custom properties a set of sources uses. */
export interface Usage {
  classes: Set<string>;
  prefixes: string[];
  /** Every `--name` the sources mention, for tokens set or read from script. */
  properties: Set<string>;
}

/** Scans source texts (TypeScript, HTML) for the `bc-*` classes and `--*` properties they use. */
export function scan(sources: Iterable<string>): Usage {
  const usage: Usage = {
    classes: new Set(),
    prefixes: [],
    properties: new Set(),
  };
  for (const text of sources) {
    for (const [name] of text.matchAll(/\bbc-[\w-]*/g))
      if (name.endsWith("-")) usage.prefixes.push(name);
      else usage.classes.add(name);
    for (const [name] of text.matchAll(/--[\w-]+/g)) usage.properties.add(name);
  }
  return usage;
}

/**
 * Keeps the parts of `sheets` (the tokens and the components, cut together so a token survives
 * while any kept rule reads it) that the scanned sources use and `provided` stylesheets do not set.
 */
export function prune(
  sheets: readonly string[],
  usage: Usage,
  provided: Iterable<string> = [],
): string[] {
  const used = (name: string) =>
    usage.classes.has(name) || usage.prefixes.some((p) => name.startsWith(p));
  const taken = new Map<string, Set<string>>();
  const ownKeyframes = new Set<string>();
  for (const sheet of provided) {
    const nodes = parse(stripComments(sheet));
    for (const [key, properties] of settings(nodes, "")) {
      const set = taken.get(key) ?? new Set();
      for (const property of properties) set.add(property);
      taken.set(key, set);
    }
    for (const node of nodes) {
      const name = node.kind === "raw" && keyframesName(node.prelude);
      if (name) ownKeyframes.add(name);
    }
  }
  const trees = sheets.map((css) =>
    keepSelectors(parse(stripComments(css)), used, taken, ""),
  );

  // Tokens: keep a custom property while a kept declaration, a kept token or a source reads it.
  const live = new Set(usage.properties);
  const declarations = trees.flatMap(collect);
  const reads = (value: string) =>
    [...value.matchAll(/var\(\s*(--[\w-]+)/g)].map((m) => m[1]);
  for (const [name, value] of declarations)
    if (!name.startsWith("--")) for (const read of reads(value)) live.add(read);
  for (let grew = true; grew;) {
    grew = false;
    for (const [name, value] of declarations)
      if (name.startsWith("--") && live.has(name))
        for (const read of reads(value))
          if (!live.has(read)) {
            live.add(read);
            grew = true;
          }
  }
  const kept = trees.map((nodes) => keepProperties(nodes, live));

  // Keyframes and registered properties follow what the kept declarations name.
  const animations = kept
    .flatMap(collect)
    .filter(([name]) => /^animation(-name)?$/.test(name))
    .map(([, value]) => value)
    .join(" ");
  return kept.map((nodes) =>
    serialize(
      nodes.filter((node) => {
        if (node.kind !== "raw") return true;
        const keyframes = keyframesName(node.prelude);
        if (keyframes)
          return (
            !ownKeyframes.has(keyframes) &&
            new RegExp(`(^|[\\s,])${keyframes}($|[\\s,])`).test(animations)
          );
        const property = node.prelude.match(/^@property\s+(--[\w-]+)/);
        if (property) return live.has(property[1]);
        return true;
      }),
    ),
  );
}

function keyframesName(prelude: string): string | null {
  return prelude.match(/^@keyframes\s+([\w-]+)/)?.[1] ?? null;
}

/** One spelling for a selector or an at-rule prelude, however it was formatted. */
function normalize(text: string): string {
  return text
    .trim()
    .replace(/\s+/g, " ")
    .replace(/\s*([>+~,])\s*/g, "$1")
    .replace(/\(\s+/g, "(")
    .replace(/\s+\)/g, ")")
    .replace(/:\s+/g, ":");
}

function stripComments(css: string): string {
  let out = "";
  for (let i = 0; i < css.length; i++) {
    const ch = css[i];
    if (ch === '"' || ch === "'") {
      const end = closeString(css, i);
      out += css.slice(i, end);
      i = end - 1;
    } else if (ch === "/" && css[i + 1] === "*") {
      const end = css.indexOf("*/", i + 2);
      i = end < 0 ? css.length : end + 1;
    } else out += ch;
  }
  return out;
}

/** The index just past the string literal opening at `start`. */
function closeString(css: string, start: number): number {
  const quote = css[start];
  let i = start + 1;
  while (i < css.length && css[i] !== quote) i += css[i] === "\\" ? 2 : 1;
  return i + 1;
}

/** The index of the `}` that closes the block whose body starts at `start`. */
function closeBlock(css: string, start: number): number {
  let depth = 0;
  for (let i = start; i < css.length; i++) {
    const ch = css[i];
    if (ch === '"' || ch === "'") i = closeString(css, i) - 1;
    else if (ch === "{") depth++;
    else if (ch === "}" && depth-- === 0) return i;
  }
  throw new Error("design-system/prune: unbalanced braces");
}

function parse(css: string): Node[] {
  const nodes: Node[] = [];
  let i = 0;
  while (i < css.length) {
    let j = i;
    while (j < css.length && css[j] !== "{" && css[j] !== ";") {
      if (css[j] === '"' || css[j] === "'") j = closeString(css, j);
      else j++;
    }
    const prelude = normalize(css.slice(i, j));
    if (j >= css.length) break;
    if (css[j] === ";") {
      if (prelude) nodes.push({ kind: "raw", prelude, body: null });
      i = j + 1;
      continue;
    }
    const end = closeBlock(css, j + 1);
    const body = css.slice(j + 1, end);
    if (/^@(media|supports|container|layer)\b/.test(prelude))
      nodes.push({ kind: "block", prelude, children: parse(body) });
    else if (prelude.startsWith("@"))
      nodes.push({ kind: "raw", prelude, body: body.trim() });
    else nodes.push({ kind: "rule", selector: prelude, body: body.trim() });
    i = end + 1;
  }
  return nodes;
}

/** Splits on `separator` outside parentheses, brackets and strings. */
function splitTop(text: string, separator: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === '"' || ch === "'") i = closeString(text, i) - 1;
    else if (ch === "(" || ch === "[") depth++;
    else if (ch === ")" || ch === "]") depth--;
    else if (ch === separator && depth === 0) {
      parts.push(text.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(text.slice(start));
  return parts.map((part) => part.trim()).filter(Boolean);
}

/** The `bc-*` classes a selector requires: those outside `:not(…)`. */
function required(selector: string): string[] {
  let bare = selector;
  for (let at = bare.indexOf(":not("); at >= 0; at = bare.indexOf(":not(")) {
    let depth = 0;
    let end = at + 4;
    for (; end < bare.length; end++) {
      if (bare[end] === "(") depth++;
      else if (bare[end] === ")" && --depth === 0) break;
    }
    bare = bare.slice(0, at) + bare.slice(end + 1);
  }
  return [...bare.matchAll(/\.(bc-[\w-]+)/g)].map((m) => m[1]);
}

/** Each selector a stylesheet styles, under its at-rule context, with the properties it sets. */
function settings(nodes: Node[], context: string): [string, string[]][] {
  return nodes.flatMap((node): [string, string[]][] => {
    if (node.kind === "block")
      return settings(node.children, `${context}${node.prelude}\n`);
    if (node.kind !== "rule") return [];
    const properties = declarations(node.body).map(([name]) => name);
    return splitTop(node.selector, ",").map((selector) => [
      context + selector,
      properties,
    ]);
  });
}

function keepSelectors(
  nodes: Node[],
  used: (name: string) => boolean,
  taken: Map<string, Set<string>>,
  context: string,
): Node[] {
  const kept: Node[] = [];
  for (const node of nodes) {
    if (node.kind === "rule") {
      // Selectors that lose the same declarations to a page stay together in one rule.
      const groups = new Map<string, string[]>();
      for (const selector of splitTop(node.selector, ","))
        if (required(selector).every(used)) {
          const lost = [...(taken.get(context + selector) ?? [])].sort();
          const key = lost.join(";");
          groups.set(key, [...(groups.get(key) ?? []), selector]);
        }
      for (const [key, selectors] of groups) {
        const lost = new Set(key ? key.split(";") : []);
        const body = declarations(node.body)
          .filter(([name]) => !lost.has(name))
          .map(([name, value]) => `${name}:${value}`)
          .join(";");
        if (body)
          kept.push({ kind: "rule", selector: selectors.join(","), body });
      }
    } else if (node.kind === "block") {
      const children = keepSelectors(
        node.children,
        used,
        taken,
        `${context}${node.prelude}\n`,
      );
      if (children.length) kept.push({ ...node, children });
    } else kept.push(node);
  }
  return kept;
}

/** Every declaration in the style rules, as `[property, value]`. */
function collect(nodes: Node[]): [string, string][] {
  return nodes.flatMap((node) =>
    node.kind === "rule"
      ? declarations(node.body)
      : node.kind === "block"
        ? collect(node.children)
        : [],
  );
}

function declarations(body: string): [string, string][] {
  return splitTop(body, ";").map((declaration) => {
    const colon = declaration.indexOf(":");
    return [
      declaration.slice(0, colon).trim(),
      declaration.slice(colon + 1).trim(),
    ];
  });
}

function keepProperties(nodes: Node[], live: Set<string>): Node[] {
  const kept: Node[] = [];
  for (const node of nodes) {
    if (node.kind === "rule") {
      const body = declarations(node.body)
        .filter(([name]) => !name.startsWith("--") || live.has(name))
        .map(([name, value]) => `${name}:${value}`)
        .join(";");
      if (body) kept.push({ ...node, body });
    } else if (node.kind === "block") {
      const children = keepProperties(node.children, live);
      if (children.length) kept.push({ ...node, children });
    } else kept.push(node);
  }
  return kept;
}

function serialize(nodes: Node[]): string {
  return nodes
    .map((node) =>
      node.kind === "rule"
        ? `${node.selector}{${node.body}}`
        : node.kind === "block"
          ? `${node.prelude}{\n${serialize(node.children)}\n}`
          : node.body === null
            ? `${node.prelude};`
            : `${node.prelude}{${node.body}}`,
    )
    .join("\n");
}
