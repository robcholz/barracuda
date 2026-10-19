import { expect, test } from "bun:test";
import {
  DOCS_CSS,
  TOKENS_CSS,
  renderDocsTokens,
  withDocsTokens,
} from "./docs-tokens";

test("the docs' token block matches the design system's tokens", async () => {
  const style = await Bun.file(DOCS_CSS).text();
  const block = renderDocsTokens(await Bun.file(TOKENS_CSS).text());
  // Out of date: run `bun tools/web/docs-tokens.ts`.
  expect(withDocsTokens(style, block)).toBe(style);
});

test("a token the design system lacks is named", () => {
  const css = `
:root, [data-theme="light"] { --gray-1000: #09090b; --gray-200: #e1e1e5; --background-200: #f5f5f7; }
[data-theme="dark"] { --gray-1000: #fafafa; --gray-200: #27272a; --background-200: #09090b; }
:root, [data-theme] { --foreground: var(--gray-1000); --border: var(--gray-200); }
`;
  expect(() => renderDocsTokens(css)).toThrow("--gray-100");
});
