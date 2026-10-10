import { afterEach, beforeEach, expect, test } from "bun:test";
import { installBrowser } from "../../../../captive-portal/resources/web/tests/browser";
import { highlight } from "../markdown/highlight";
import { heal, markdownSource, renderMarkdown, trailing } from "../markdown";

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

test("a mark left open in an earlier paragraph stays literal; an open fence runs to the end", () => {
  const root = paint("先 **加粗\n\n```rust\nfn main() {");
  expect(root.querySelector("p")!.textContent).toBe("先 **加粗");
  expect(root.querySelector("strong")).toBeNull();
  const code = root.querySelector(".bc-md-code")!;
  expect(code.querySelector(".bc-md-code__head")!.textContent).toBe("rust");
  expect(code.querySelector("pre code")!.textContent).toBe("fn main() {");
  expect(code.querySelector(".bc-tok-k")!.textContent).toBe("fn");
});

test("painting again keeps unchanged blocks and patches the one that grew", () => {
  const root = paint("第一段\n\n第二");
  const first = root.firstElementChild;
  const second = root.lastElementChild!;
  const text = second.firstChild!;
  // a selection in the growing text survives the next paint
  const range = document.createRange();
  range.setStart(text, 0);
  range.setEnd(text, 2);
  paint("第一段\n\n第二段", root);
  expect(root.firstElementChild).toBe(first);
  expect(root.lastElementChild).toBe(second);
  expect(second.firstChild).toBe(text);
  expect(text.textContent).toBe("第二段");
  expect([range.startOffset, range.endOffset]).toEqual([0, 2]);
  expect(markdownSource(root)).toBe("第一段\n\n第二段");
});

/** A reply as a model streams it, touching every block and mark. */
const SAMPLE = `## 让 LED 闪烁

在 **ESP32** 上用 \`gpio\` 控制 LED，*每 500 ms* 切换一次，snake_case 不受影响。详见 [文档](https://example.com/docs)。

\`\`\`rust
async fn blink(mut led: Output<'static>) {
    loop {
        led.toggle(); // 翻转
    }
}
\`\`\`

1. 编译：\`cargo build --release\`
2. 烧录
   - 连接 **USB** 线
   - [x] 选择端口

> 注意：~~旧引脚~~ 以丝印为准。

---

| 引脚 | 功能 | 电压 |
| --- | :-: | ---: |
| GPIO2 | LED | 3.3 V |

完成 2 * 3 = 6。`;

/** Marks that nest, a link whose URL holds parentheses, fences inside a list, lists inside a quote. */
const TRICKY = `**注意**：先读 *[链接](https://a.com/x_(y))* 再看 \`a*b\` 和 __粗__。

- 项目 *一*
  1. 子 \`code\`
  2. 子二

  \`\`\`python
  def f(x): return x ** 2  # 平方
  \`\`\`
- 下一项 ~~删除~~

### 小结 **重点**

1) 第一
2) 第二

> - 引用里的列表
> - 第二项 **粗体**

价格 $5 * 2，结尾。`;

/**
 * Streams `source` a character at a time and checks that no paint takes back text an earlier one
 * showed, and that a code block keeps its elements; returns the root and what it showed last.
 */
function stream(source: string) {
  const root = document.createElement("div");
  let shown = "";
  let pre: Element | null = null;
  let copy: Element | null = null;
  for (let end = 1; end <= source.length; end += 1) {
    renderMarkdown(root, source.slice(0, end), {
      streaming: true,
      codeActions: () => document.createElement("button"),
    });
    const text = (root.textContent ?? "").replace(/\s+/g, "");
    if (!text.startsWith(shown))
      throw new Error(
        `after ${JSON.stringify(source.slice(end - 12, end))}: ${JSON.stringify(shown.slice(-12))} became ${JSON.stringify(text.slice(-12))}`,
      );
    shown = text;
    const code: Element | null = root.querySelector("pre");
    if (pre && code) expect(code).toBe(pre);
    pre ??= code;
    const button: Element | null = root.querySelector("button");
    if (copy && button) expect(button).toBe(copy);
    copy ??= button;
  }
  // the whole text painted as written draws what it showed
  renderMarkdown(root, source);
  expect((root.textContent ?? "").replace(/\s+/g, "")).toBe(shown);
  return { root, shown };
}

test("streamed a character at a time, what is shown is never taken back", () => {
  const { root, shown } = stream(SAMPLE);
  // no raw mark or link syntax ever showed
  expect(shown).not.toMatch(/\*\*|~~|\]\(|`|\[x\]/);
  expect([...root.children].map((node) => node.tagName)).toEqual([
    "H2",
    "P",
    "DIV",
    "OL",
    "BLOCKQUOTE",
    "HR",
    "DIV",
    "P",
  ]);
  const tricky = stream(TRICKY).root;
  expect(tricky.querySelector("em > a")!.getAttribute("href")).toBe(
    "https://a.com/x_(y)",
  );
  expect(tricky.querySelector("li pre")!.textContent).toContain("x ** 2");
  expect(tricky.querySelector("blockquote li strong")!.textContent).toBe(
    "粗体",
  );
});

test("a streaming tail is shown provisionally", () => {
  const shown = (source: string) => {
    const root = document.createElement("div");
    renderMarkdown(root, source, { streaming: true });
    return root;
  };
  expect(shown("先 **加").querySelector("strong")!.textContent).toBe("加");
  expect(shown("先 *斜 **粗").innerHTML).toBe(
    "<p>先 <em>斜 <strong>粗</strong></em></p>",
  );
  expect(shown("运行 `mak").querySelector("code")!.textContent).toBe("mak");
  expect(shown("先 **").textContent).toBe("先");
  expect(shown("见 [文档](https://exa").textContent).toBe("见 文档");
  // a link on its way is drawn as one, without a target until its URL is whole
  const coming = shown("见 [文档](https://exa").querySelector("a")!;
  expect(coming.hasAttribute("href")).toBe(false);
  expect(coming.hasAttribute("data-incomplete")).toBe(true);
  expect(shown("见 [文").querySelector("a[data-incomplete]")!.textContent).toBe(
    "文",
  );
  expect(shown("见 [文档]").querySelector("a[data-incomplete]")).not.toBeNull();
  expect(shown("见 ![图").textContent).toBe("见 图");
  // a line that is only a marker waits; so does a fence's line and a table's header
  for (const tail of [
    "#",
    "- ",
    "1",
    "12.",
    "``",
    "> ",
    "- [x",
    "```ru",
    "| a | b |",
  ])
    expect(shown(`段落\n\n${tail}`).textContent).toBe("段落");
  // a delimiter row on its way makes the table
  const table = shown("| a | b |\n| -");
  expect(table.querySelectorAll("th")).toHaveLength(2);
  // inside a fence, the closing backticks wait
  expect(shown("```js\nx\n``").querySelector("code")!.textContent).toBe("x");
  // a finished reply is healed the same way, so nothing changes the moment it ends
  const written = document.createElement("div");
  renderMarkdown(written, "先 **加");
  expect(written.querySelector("strong")!.textContent).toBe("加");
  // but its last line is drawn whole: what waited while streaming shows
  renderMarkdown(written, "段落\n\n1");
  expect(written.textContent).toBe("段落1");
});

test("healing closes only the last paragraph, never into code", () => {
  // remend 1.4.0 appends `**` to the end in both
  expect(heal("先 **加粗\n\n新段落")).toBe("先 **加粗\n\n新段落");
  expect(heal("先 **加粗\n\n```rust\nfn main() {")).toBe(
    "先 **加粗\n\n```rust\nfn main() {",
  );
  expect(heal("新段落 **加粗\n\n")).toBe("新段落 **加粗**\n\n");
  // a blank line inside a fence does not end the paragraph after it
  expect(heal("```\na\n\nb\n```\n然后 **加")).toBe(
    "```\na\n\nb\n```\n然后 **加**",
  );
});

test("a bare URL ends at CJK punctuation", () => {
  const root = paint(
    "请访问 https://example.com/a。谢谢，或 https://b.cn（备用）",
  );
  expect(
    [...root.querySelectorAll("a")].map((a) => a.getAttribute("href")),
  ).toEqual(["https://example.com/a", "https://b.cn"]);
  expect(root.textContent).toBe(
    "请访问 https://example.com/a。谢谢，或 https://b.cn（备用）",
  );
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
