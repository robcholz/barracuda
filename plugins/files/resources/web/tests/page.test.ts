import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  installBrowser,
  settle,
} from "../../../../captive-portal/resources/web/tests/browser";
import type {
  PortalContext,
  Toast,
} from "../../../../captive-portal/resources/web/ui";
import {
  encodePath,
  formatSize,
  groupPlugins,
  mount,
  nameError,
  type Listing,
} from "../entry";

const json = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
const listing = (...entries: Listing["entries"]): Listing => ({
  entries,
  truncated: false,
});
const dir = (name: string) => ({ name, type: "dir" as const, size: 0 });
const file = (name: string, size: number) => ({
  name,
  type: "file" as const,
  size,
});

let browser: ReturnType<typeof installBrowser>;
let calls: { url: string; method: string; body?: string }[];
let lists: Record<string, Listing>;
const realFetch = globalThis.fetch;

beforeEach(() => {
  browser = installBrowser();
  calls = [];
  lists = {
    "/workspace/media": listing(dir("agent-runs"), file("notes.txt", 640)),
    "/workspace/removable": listing(),
    "/plugins/data": listing(dir("agent"), dir("workflow")),
    "/plugins/cache": listing(dir("agent")),
    "/plugins/media": listing(),
    "/plugins/resources": listing(dir("captive-portal")),
  };
  globalThis.fetch = Object.assign(
    async (url: RequestInfo | URL, init: RequestInit = {}) => {
      const method = init.method ?? "GET";
      calls.push({
        url: String(url),
        method,
        body: init.body as string | undefined,
      });
      const list = /^\/api\/files\/list\/(.+)$/.exec(String(url));
      if (list) {
        const path = `/${list[1].split("/").map(decodeURIComponent).join("/")}`;
        return lists[path]
          ? json(lists[path])
          : json({ error: "not_found" }, 404);
      }
      if (String(url) === "/api/files/mkdir") return json(null, 201);
      return new Response(null, { status: 404 });
    },
    { preconnect: realFetch.preconnect },
  );
});
afterEach(async () => {
  globalThis.fetch = realFetch;
  await browser.close();
});

async function render() {
  const controller = new AbortController();
  const toasts: Toast[] = [];
  const context: PortalContext = {
    signal: controller.signal,
    lang: "zh",
    toast: (toast) => toasts.push(toast),
    navigate: () => {},
    status: () => null,
    refreshStatus: async () => {},
  };
  const root = browser.document.createElement("div");
  browser.document.body.append(root);
  const cleanup = mount(root, context) as () => void;
  await settle(20);
  const rows = () =>
    [...root.querySelectorAll<HTMLElement>(".bc-tree .bc-nav-item")].map(
      (row) => row.querySelector(".bc-tree__name")?.textContent,
    );
  const row = (name: string) =>
    [...root.querySelectorAll<HTMLElement>(".bc-tree .bc-nav-item")].find(
      (element) =>
        element.querySelector(".bc-tree__name")?.textContent === name,
    )!;
  const click = async (name: string) => {
    row(name).click();
    await settle(20);
  };
  const buttons = () =>
    [
      ...root.querySelectorAll<HTMLButtonElement>(".bc-pane__bar .bc-button"),
    ].map((element) => element.textContent?.trim());
  return { root, toasts, controller, cleanup, rows, row, click, buttons };
}

test("paths are encoded one name at a time", () => {
  expect(encodePath("/workspace/media/a b/✓#?.txt")).toBe(
    "workspace/media/a%20b/%E2%9C%93%23%3F.txt",
  );
});

test("sizes read as the design writes them", () => {
  expect(formatSize(812)).toBe("812 B");
  expect(formatSize(2458)).toBe("2.4 KB");
  expect(formatSize(38 * 1024)).toBe("38 KB");
  expect(formatSize(2.3 * 1024 * 1024)).toBe("2.3 MB");
});

test("a new name is refused when empty, a path, or taken", () => {
  expect(nameError("", [], "a")).toBe("errEmpty");
  expect(nameError("a/b", [], "a")).toBe("errSlash");
  expect(nameError("a\\b", [], "a")).toBe("errSlash");
  expect(nameError("..", [], "a")).toBe("errSlash");
  expect(nameError("b", ["a", "b"], "a")).toBe("errDup");
  expect(nameError("a", ["a"], "a")).toBeNull();
});

test("Plugin trees are grouped by Plugin", () => {
  expect([
    ...groupPlugins([
      ["data", [dir("workflow"), dir("agent"), file("stray", 1)]],
      ["cache", [dir("agent")]],
    ]),
  ]).toEqual([
    ["agent", ["data", "cache"]],
    ["workflow", ["data"]],
  ]);
});

test("the tree opens on 媒体 and shows every place", async () => {
  const page = await render();
  expect(page.rows()).toEqual([
    "媒体",
    "agent-runs",
    "notes.txt",
    "缓存",
    "资源",
    "存储卡",
    "插件",
  ]);
  expect(page.root.querySelector(".bc-kv dd")?.textContent).toBe("未插入");
  expect(page.buttons()).toEqual(["新建文件夹", "上传文件"]);
  expect(browser.document.head.textContent).toContain(".bc-tree{");
  page.cleanup();
  expect(browser.document.head.textContent).not.toContain(".bc-tree{");
});

test("插件 lists each Plugin with its trees, and offers no change", async () => {
  const page = await render();
  await page.click("插件");
  expect(page.rows().slice(-3)).toEqual([
    "agent",
    "captive-portal",
    "workflow",
  ]);
  await page.click("agent");
  expect(page.rows().slice(-5)).toEqual([
    "agent",
    "data",
    "cache",
    "captive-portal",
    "workflow",
  ]);
  lists["/plugins/data/agent"] = listing(file("notes.json", 12));
  await page.click("data");
  expect(page.rows()).toContain("notes.json");
  expect(page.buttons()).toEqual([]);
  expect(page.root.querySelector(".bc-pane .bc-term")?.textContent).toContain(
    "只读",
  );
  page.cleanup();
});

test("a missing card is said, and its place has nothing to change", async () => {
  const page = await render();
  await page.click("存储卡");
  expect(page.root.querySelector(".bc-pane")?.textContent).toContain(
    "未插入存储卡",
  );
  page.cleanup();
});

test("one inserted card is the place itself", async () => {
  lists["/workspace/removable"] = listing(dir("micro-sd"));
  lists["/workspace/removable/micro-sd"] = listing(file("log.csv", 1300));
  const page = await render();
  expect(page.root.querySelector(".bc-kv dd")?.textContent).toBe(
    "micro-sd · 已插入",
  );
  await page.click("存储卡");
  expect(page.rows()).toContain("log.csv");
  expect(page.buttons()).toEqual(["新建文件夹", "上传文件"]);
  page.cleanup();
});

test("新建文件夹 creates a free name and opens its rename", async () => {
  lists["/workspace/media"] = listing(dir("untitled"), file("notes.txt", 1));
  const page = await render();
  lists["/workspace/media"] = listing(
    dir("untitled"),
    dir("untitled-2"),
    file("notes.txt", 1),
  );
  [...page.root.querySelectorAll<HTMLButtonElement>("button")]
    .find((element) => element.textContent?.trim() === "新建文件夹")!
    .click();
  await settle(30);
  const mkdir = calls.find((call) => call.url === "/api/files/mkdir");
  expect(JSON.parse(mkdir?.body ?? "{}")).toEqual({
    path: "/workspace/media/untitled-2",
  });
  expect(
    page.root.querySelector<HTMLInputElement>(".bc-pane__section input")?.value,
  ).toBe("untitled-2");
  page.cleanup();
});
