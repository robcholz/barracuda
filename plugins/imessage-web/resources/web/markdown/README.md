# Web chat Markdown

Streaming Markdown for chat replies: the rendering logic of Vercel's
[Streamdown](https://github.com/vercel/streamdown), rebuilt for the DOM without
React, remark, rehype or Shiki, small enough to ship from the device (about 29 KB
minified). It imports nothing outside this directory. Web chat
(`../entry.ts`) is its one user.

```ts
import {
  MARKDOWN_CSS,
  markdownSource,
  renderMarkdown,
  trailing,
} from "./markdown";

style.textContent += MARKDOWN_CSS;
const reply = document.createElement("div");
reply.className = "bc-reply bc-md";

// on every delta, and once more when the reply ends
renderMarkdown(reply, text, {
  streaming: true, // false for the last paint
  keep: caret, // the page's caret: painting leaves it where it is
  codeActions: (read) => copyButton(read), // read() is the block's code now
});
trailing(reply, caret).append(caret); // the end of the last line
markdownSource(reply); // the Markdown last painted, for Copy and Reply
```

## What it guarantees while a reply streams

**Nothing shown is taken back.** Streamed one character at a time, every paint's
text extends the last one's. No `**`, backtick, `[x]` or raw URL appears and
then vanishes, and no line switches from text to a list, heading or table. The
tests check this on every frame of two replies covering every block and mark
(`../tests/markdown.test.ts`).

**The DOM is patched, not replaced.** Elements and text nodes that stay are kept,
so a selection, a hovered Copy button and a code block's horizontal scroll all
survive each paint.

## How it works

| Step                                                                                                                                                                                                                                                               | File                    | From Streamdown                                       |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ----------------------- | ----------------------------------------------------- |
| Heal what the last paragraph leaves open: `**bol` closes, `` `co `` closes, `[label](https://ex` becomes a link still on its way, and `- > 25` is not a quote                                                                                                      | `heal.ts`               | ported from remend 1.4.0                              |
| While streaming: a last line that is only a block marker (`#`, `-`, `1.`, `` ` ``, `- [x`) waits; a table header waits for its delimiter row; a fence's own line and closing backticks wait; a trailing mark run waits until it is known to open, close or be text | `stream.ts`             | this package's own; remend handles only inline marks  |
| Split into top-level blocks; a block whose source is unchanged keeps its node                                                                                                                                                                                      | `index.ts`, `blocks.ts` | block memoization                                     |
| Patch the block that changed in place                                                                                                                                                                                                                              | `dom.ts`                | React's reconciliation, in about 80 lines             |
| A code block whose fence is still open is `data-incomplete`: its actions rest, and the caret hides after it (and after a table)                                                                                                                                    | `index.ts`, `style.ts`  | the incomplete code block and the caret's hiding rule |
| A bare URL ends at CJK punctuation (`https://example.com。谢谢`)                                                                                                                                                                                                   | `inline.ts`             | streamdown-cjk's autolink boundaries                  |
| Colour code by its fence's language                                                                                                                                                                                                                                | `highlight.ts`          | (Shiki's job there) a small scanner here              |

A finished reply is healed too, as Streamdown heals in its streaming mode, so
nothing changes the moment it ends. Only the last-line waits drop out.

### Where it differs from remend

`heal.ts` fixes three remend 1.4.0 behaviours that show up as flashes:

- remend appends closers at the very end of the text. A mark left open in an
  earlier paragraph was closed in the last one, or inside an open code block
  (`**a\n\nb` became `**a\n\nb**`). `heal` closes only the last paragraph, and
  puts closers before trailing whitespace.
- A link's URL counted as whole at its first `)`. `[x](https://a.com/x_(y)` showed
  raw. Now the URL is whole only once its parentheses balance.
- Once a link was healed, remend stopped. Marks around the link stayed open
  (`*[链接](https://ex` showed its `*`), and so did marks inside its label. `heal`
  heals both.

remend's handlers for KaTeX, HTML tags, setext headings and single tildes, its
options and its custom handlers are left out. This renderer draws `$`, HTML and
setext underlines as text, and has no single-tilde strikethrough.

### What is not ported

- word fade-in animation: Web chat already reveals text a few characters per frame;
- Mermaid, KaTeX and raw HTML;
- the link-safety modal: links open only `http(s):` and `mailto:`, in a new tab;
- table and code download menus;
- right-to-left detection;
- Shiki's incremental tokenizer: `highlight.ts` re-scans a growing block. In
  Chromium that takes about 0.9 ms for 5 KB of code, and a whole streaming paint
  of a reply ending in a 5 KB code block takes about 4 ms, well inside a frame.

## Markdown it draws

- Paragraphs, with single newlines kept as line breaks.
- ATX headings and thematic breaks.
- Fenced code with a language label.
- Bullet, ordered and task lists, nested by any indent past the marker.
- Block quotes.
- GFM tables with alignment.
- Inline code, strong, emphasis and strikethrough.
- Links, autolinks and bare URLs.
- Images, drawn as links named by their alt text: a reply loads nothing.

Nodes are built and never parsed from markup, so HTML in a reply stays text.

## Styles

`MARKDOWN_CSS` scopes everything to `.bc-md`, `.bc-md-*` and `.bc-tok-*`. It reads
the portal design system's tokens, so it follows the light and dark themes.

## Tests

- `../tests/markdown.test.ts`: rendering, streaming and highlighting.
- `../tests/heal/`: remend's own suites, run against `heal.ts` (313 cases).
- `../tests/chat.test.ts`: the package inside Web chat.

## License

`heal.ts` and `../tests/heal/` derive from Streamdown, Copyright 2023 Vercel,
Inc., under the Apache License 2.0 (`APACHE-2.0.txt`). See `NOTICE`.
