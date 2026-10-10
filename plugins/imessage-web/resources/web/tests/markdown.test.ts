import { afterEach, beforeEach, expect, test } from "bun:test";
import { installBrowser } from "../../../../captive-portal/resources/web/tests/browser";
import { highlight } from "../highlight";
import { markdownSource, renderMarkdown, trailing } from "../markdown";

let browser: ReturnType<typeof installBrowser>;

beforeEach(() => {
  browser = installBrowser();
});
afterEach(async () => {
  await browser.close();
});

function paint(source: string, target = document.createElement("div")) {
  renderMarkdown(target, source, {
    codeActions: () => document.createElement("button"),
  });
  return target;
}

/** The highlighted tokens of `code`, as `kind:text`. */
function tokens(code: string, lang: string) {
  return highlight(code, lang)!
    .filter((node): node is HTMLElement => node instanceof HTMLElement)
    .map(
      (node) => `${node.className.slice("bc-tok-".length)}:${node.textContent}`,
    );
}

test("renders the blocks a model writes", () => {
  const root = paint(
    [
      "## 结果",
      "",
      "第一行",
      "第二行",
      "",
      "- 一",
      "  - 嵌套",
      "- [x] 完成",
      "",
      "3. 三",
      "4. 四",
      "",
      "> 引用",
      "",
      "---",
      "",
      "| 名称 | 值 |",
      "| :--- | --: |",
      "| `a|b` | 2 |",
    ].join("\n"),
  );
  const tags = [...root.children].map((node) => node.tagName);
  expect(tags).toEqual(["H2", "P", "UL", "OL", "BLOCKQUOTE", "HR", "DIV"]);
  // a single newline stays a line break, as the reply's pre-wrap shows it
  expect(root.querySelector("p")!.textContent).toBe("第一行\n第二行");
  expect(root.querySelector("ul > li > ul > li")!.textContent).toBe("嵌套");
  const task = root.querySelector<HTMLInputElement>(".bc-md-task input")!;
  expect(task.checked).toBe(true);
  expect(task.disabled).toBe(true);
  expect(root.querySelector("ol")!.getAttribute("start")).toBe("3");
  expect(root.querySelectorAll("ol > li")).toHaveLength(2);
  expect(root.querySelector("blockquote > p")!.textContent).toBe("引用");
  const cells = [...root.querySelectorAll("td")];
  expect(cells.map((cell) => cell.textContent)).toEqual(["a|b", "2"]);
  expect(cells[1].style.textAlign).toBe("right");
});

test("inline marks, links and raw HTML", () => {
  const root = paint(
    "**粗** *斜* ***两者*** ~~删~~ `<b>` snake_case_name [文档](https://example.com/a_(b)) " +
      "[坏](javascript:alert(1)) 见 https://example.com/x. <img src=x onerror=alert(1)>",
  );
  const p = root.querySelector("p")!;
  expect(p.querySelector("strong")!.textContent).toBe("粗");
  expect(p.querySelector(":scope > em")!.textContent).toBe("斜");
  expect(p.querySelector("strong > em")!.textContent).toBe("两者");
  expect(p.querySelector("del")!.textContent).toBe("删");
  expect(p.querySelector("code")!.textContent).toBe("<b>");
  expect(p.textContent).toContain("snake_case_name");
  const links = [...p.querySelectorAll("a")];
  expect(links.map((a) => a.getAttribute("href"))).toEqual([
    "https://example.com/a_(b)",
    "https://example.com/x",
  ]);
  expect(links[0].getAttribute("target")).toBe("_blank");
  expect(links[0].getAttribute("rel")).toBe("noopener noreferrer");
  // an unsafe link keeps only its label; markup stays text
  expect(p.textContent).toContain("坏");
  expect(p.textContent).toContain("<img src=x onerror=alert(1)>");
  expect(root.querySelector("img, b")).toBeNull();
});

test("an unclosed mark stays literal; an unclosed fence runs to the end", () => {
  const root = paint("先 **加粗\n\n```rust\nfn main() {");
  expect(root.querySelector("p")!.textContent).toBe("先 **加粗");
  expect(root.querySelector("strong")).toBeNull();
  const code = root.querySelector(".bc-md-code")!;
  expect(code.querySelector(".bc-md-code__head")!.textContent).toBe("rust");
  expect(code.querySelector("pre code")!.textContent).toBe("fn main() {");
  expect(code.querySelector(".bc-tok-k")!.textContent).toBe("fn");
});

test("painting again keeps the blocks whose source is unchanged", () => {
  const root = paint("第一段\n\n第二");
  const first = root.firstElementChild;
  const second = root.lastElementChild;
  paint("第一段\n\n第二段", root);
  expect(root.firstElementChild).toBe(first);
  expect(root.lastElementChild).not.toBe(second);
  expect(root.lastElementChild!.textContent).toBe("第二段");
  expect(markdownSource(root)).toBe("第一段\n\n第二段");
});

test("the trailing element is the last line's block", () => {
  expect(trailing(paint("a\n\n- b\n- c")).textContent).toBe("c");
  const code = paint("```\nx\n```");
  expect(trailing(code)).toBe(code);
});

test("highlights code by its fence's language", () => {
  expect(
    tokens('let s = String::from("hi"); // note\nprintln!("{}", 1.5);', "rust"),
  ).toEqual([
    "k:let",
    "t:String",
    "f:from",
    's:"hi"',
    "c:// note",
    "f:println!",
    's:"{}"',
    "n:1.5",
  ]);
  // a lifetime is not a string
  expect(tokens("fn f<'a>(x: &'a str) -> char { 'x' }", "rs")).toEqual([
    "k:fn",
    "s:'x'",
  ]);
  expect(
    tokens('def f():\n    """doc"""\n    return None # done', "python"),
  ).toEqual(["k:def", "f:f", 's:"""doc"""', "k:return", "n:None", "c:# done"]);
  expect(tokens("#include <stdio.h>\nint x = 0x1F; /* c */", "c")).toEqual([
    "k:#include",
    "k:int",
    "n:0x1F",
    "c:/* c */",
  ]);
  expect(tokens('echo "$HOME" a#b $1 # end', "bash")).toEqual([
    's:"$HOME"',
    "v:$1",
    "c:# end",
  ]);
  expect(tokens("SELECT id FROM t -- all", "SQL")).toEqual([
    "k:SELECT",
    "k:FROM",
    "c:-- all",
  ]);
  expect(tokens("@@ -1 +1 @@\n-old\n+new\n", "diff")).toEqual([
    "f:@@ -1 +1 @@\n",
    "d:-old\n",
    "s:+new\n",
  ]);
  // the text joins back to the code; an unknown language is left plain
  const code = "const a = `x${1}`; // y";
  expect(
    highlight(code, "ts")!
      .map((node) => node.textContent)
      .join(""),
  ).toBe(code);
  expect(highlight("x", "brainfuck")).toBeNull();
});
