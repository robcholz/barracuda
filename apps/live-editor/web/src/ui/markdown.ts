/**
 * The agent's replies as the design system's chat shows them: paragraphs as `.bc-reply`, fenced
 * code as `.bc-code`, inline code as `.bc-inline-code`, lists, bold and links. Everything is built
 * as DOM nodes from text; nothing the model writes is parsed as HTML.
 */

import { h, type Child } from "./dom";

/** Inline marks: `code`, **bold** and [links](https://…). */
export function inline(text: string): Child[] {
  const out: Child[] = [];
  const pattern =
    /`([^`\n]+)`|\*\*([^*\n]+)\*\*|\[([^\]\n]+)\]\((https?:\/\/[^)\s]+)\)/g;
  let at = 0;
  for (const match of text.matchAll(pattern)) {
    if (match.index > at) out.push(text.slice(at, match.index));
    if (match[1] !== undefined)
      out.push(h("span", { class: "bc-inline-code" }, match[1]));
    else if (match[2] !== undefined) out.push(h("strong", null, match[2]));
    else
      out.push(
        h(
          "a",
          {
            class: "bc-link bc-link--prose",
            href: match[4],
            target: "_blank",
            rel: "noopener noreferrer",
          },
          match[3],
        ),
      );
    at = match.index + match[0].length;
  }
  if (at < text.length) out.push(text.slice(at));
  return out;
}

/** Renders `text` as blocks. */
export function markdown(text: string): HTMLElement {
  const root = h("div", { class: "le-reply" });
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  let paragraph: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  const flush = () => {
    if (paragraph.length)
      root.append(h("p", { class: "bc-reply" }, inline(paragraph.join("\n"))));
    paragraph = [];
    if (list)
      root.append(
        h(
          list.ordered ? "ol" : "ul",
          { class: "bc-reply" },
          list.items.map((item) => h("li", null, inline(item))),
        ),
      );
    list = null;
  };
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const fence = /^```/.exec(line);
    if (fence) {
      flush();
      const code: string[] = [];
      for (i += 1; i < lines.length && !lines[i].startsWith("```"); i += 1)
        code.push(lines[i]);
      root.append(h("pre", { class: "bc-code" }, code.join("\n")));
      continue;
    }
    const heading = /^#{1,6}\s+(.*)$/.exec(line);
    if (heading) {
      flush();
      root.append(
        h("p", { class: "bc-reply" }, h("strong", null, inline(heading[1]))),
      );
      continue;
    }
    const item = /^\s*(?:([-*])|(\d+)[.)])\s+(.*)$/.exec(line);
    if (item) {
      const ordered = item[2] !== undefined;
      if (paragraph.length || (list && list.ordered !== ordered)) flush();
      list ??= { ordered, items: [] };
      list.items.push(item[3]);
      continue;
    }
    if (line.trim() === "") {
      flush();
      continue;
    }
    if (list) flush();
    paragraph.push(line);
  }
  flush();
  return root;
}
