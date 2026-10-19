import { expect, test } from "bun:test";
import { prune, scan } from "../prune";

const TOKENS = `:root{--gray-500:#6c6c75;--gray-900:#18181b;--unused:#fff;--muted-foreground:var(--gray-500);--ring:var(--gray-900)}`;

const COMPONENTS = `
/* Button */
.bc-button { height: 40px; transition: color 0.1s }
.bc-button--ghost, .bc-tile { background: none }
.bc-button:not(.bc-button--icon) { padding: 0 16px }
.bc-picker__row input, .bc-radio-card input { appearance: none }
.bc-muted { color: var(--muted-foreground) }
.bc-caret { width: 8px; animation: bc-blink 1s step-end infinite }
[hidden] { display: none !important }
@media (max-width: 719px) {
  .bc-button { height: 44px }
  .bc-tile { padding: 0 }
}
@property --sidebar-width { syntax: "<length>"; inherits: true; initial-value: 240px }
@keyframes bc-blink { 50% { opacity: 0 } }
@keyframes bc-spin { to { transform: rotate(360deg) } }
`;

const cut = (sources: string[], provided: string[] = []) =>
  prune([TOKENS, COMPONENTS], scan(sources), provided);

test("keeps the selectors whose classes the sources use, and every unclassed rule", () => {
  const [, components] = cut([`h("button", { class: "bc-button" })`]);
  expect(components).toContain(".bc-button{height:40px");
  expect(components).toContain(
    "@media (max-width:719px){\n.bc-button{height:44px}\n}",
  );
  expect(components).toContain("[hidden]{display:none !important}");
  expect(components).not.toContain("bc-tile");
  expect(components).not.toContain("bc-button--ghost");
  // a class inside :not() is not required
  expect(components).toContain(
    ".bc-button:not(.bc-button--icon){padding:0 16px}",
  );
});

test("cuts a selector list down to its used selectors", () => {
  const [, components] = cut([`"bc-radio-card"`]);
  expect(components).toContain(".bc-radio-card input{appearance:none}");
  expect(components).not.toContain("bc-picker__row");
});

test("a name ending in a dash uses every class it starts", () => {
  const [, components] = cut(["`bc-button--${variant}`"]);
  expect(components).toContain(".bc-button--ghost{background:none}");
  expect(components).not.toContain(".bc-button{");
});

test("keeps the tokens kept rules and sources read, through other tokens", () => {
  const [tokens] = cut([`"bc-muted"`, `style: "outline-color:var(--ring)"`]);
  expect(tokens).toContain("--muted-foreground:var(--gray-500)");
  expect(tokens).toContain("--gray-500:#6c6c75");
  expect(tokens).toContain("--ring:var(--gray-900)");
  expect(tokens).toContain("--gray-900:#18181b");
  expect(tokens).not.toContain("--unused");
});

test("keeps the keyframes and registered properties kept rules name", () => {
  const [, none] = cut([`"bc-button"`]);
  expect(none).not.toContain("@keyframes");
  expect(none).not.toContain("@property");
  const [, caret] = cut([`"bc-caret"`, "--sidebar-width"]);
  expect(caret).toContain("@keyframes bc-blink");
  expect(caret).not.toContain("@keyframes bc-spin");
  expect(caret).toContain("@property --sidebar-width");
});

test("leaves to a page the declarations and keyframes its own stylesheet sets", () => {
  const page =
    ".bc-caret{animation:bc-blink 1s step-end infinite}@keyframes bc-blink{50%{opacity:0}}" +
    "@media (max-width:719px){.bc-tile{padding:0}}";
  const [, components] = cut([`"bc-caret bc-tile"`], [page]);
  expect(components).toContain(".bc-caret{width:8px}");
  expect(components).not.toContain("@keyframes bc-blink");
  // the rule's other selector keeps the declaration; the page's media rule goes whole
  expect(components).toContain(".bc-tile{background:none}");
  expect(components).not.toContain("@media");
});

test("the design system parses whole: using everything keeps every rule", async () => {
  const dir = new URL("..", import.meta.url);
  const tokens = await Bun.file(new URL("tokens.css", dir)).text();
  const components = await Bun.file(new URL("components.css", dir)).text();
  const [keptTokens, keptComponents] = prune([tokens, components], {
    classes: new Set(),
    prefixes: ["bc-"],
    properties: new Set(),
  });
  const count = (css: string) =>
    css.replace(/\/\*[\s\S]*?\*\//g, "").split("{").length;
  expect(count(keptComponents)).toBe(count(components));
  // every token is read by a rule or another token
  expect(count(keptTokens)).toBe(count(tokens));
});
