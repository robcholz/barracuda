import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  installBrowser,
  settle,
} from "../../../../captive-portal/resources/web/tests/browser";
import type {
  PortalContext,
  Toast,
} from "../../../../captive-portal/resources/web/ui";
import { APPS, asOffer, Incoming, PROTOCOL } from "../incoming";

/** The Files routes over an in-memory tree, with their refusals. */
function fakeFiles() {
  const files = new Map<string, Uint8Array>();
  const dirs = new Set(["/workspace", "/workspace/media"]);
  const parent = (path: string) => path.slice(0, path.lastIndexOf("/"));
  const children = (path: string) =>
    [...dirs, ...files.keys()].filter((entry) => parent(entry) === path);
  const json = (value: unknown, status = 200) =>
    new Response(value === null ? null : JSON.stringify(value), { status });
  const fetch = async (url: RequestInfo | URL, init: RequestInit = {}) => {
    const [, route, rest = ""] =
      /^\/api\/files\/([a-z]+)\/?(.*)$/.exec(String(url)) ?? [];
    const path = `/${rest.split("/").map(decodeURIComponent).join("/")}`;
    const body =
      typeof init.body === "string"
        ? (JSON.parse(init.body) as Record<string, string>)
        : {};
    switch (route) {
      case "list":
        if (!dirs.has(path)) return json({ error: "not_found" }, 404);
        return json({
          entries: children(path).map((entry) => ({
            name: entry.slice(path.length + 1),
            type: dirs.has(entry) ? "dir" : "file",
            size: files.get(entry)?.length ?? 0,
          })),
          truncated: false,
        });
      case "raw":
        return files.has(path)
          ? new Response(files.get(path))
          : json({ error: "not_found" }, 404);
      case "upload":
        if (!dirs.has(parent(path))) return json({ error: "not_found" }, 404);
        if (files.has(path) || dirs.has(path))
          return json({ error: "exists" }, 409);
        files.set(path, new Uint8Array(init.body as ArrayBuffer));
        return json({}, 201);
      case "mkdir":
        if (dirs.has(body.path) || files.has(body.path))
          return json({ error: "exists" }, 409);
        if (!dirs.has(parent(body.path)))
          return json({ error: "not_found" }, 404);
        dirs.add(body.path);
        return json(null, 201);
      case "rename": {
        if (!dirs.has(body.from) && !files.has(body.from))
          return json({ error: "not_found" }, 404);
        if (dirs.has(body.to) || files.has(body.to))
          return json({ error: "exists" }, 409);
        for (const dir of [...dirs].filter(
          (d) => d === body.from || d.startsWith(`${body.from}/`),
        )) {
          dirs.delete(dir);
          dirs.add(body.to + dir.slice(body.from.length));
        }
        for (const [file, bytes] of [...files].filter(
          ([f]) => f.startsWith(`${body.from}/`) || f === body.from,
        )) {
          files.delete(file);
          files.set(body.to + file.slice(body.from.length), bytes);
        }
        return json(null, 204);
      }
      case "delete":
        if (files.delete(body.path)) return json(null, 204);
        if (!dirs.has(body.path)) return json({ error: "not_found" }, 404);
        if (children(body.path).length)
          return json({ error: "not_empty" }, 409);
        dirs.delete(body.path);
        return json(null, 204);
    }
    return json({ error: "not_found" }, 404);
  };
  return { files, dirs, fetch };
}

const utf8 = (text: string) =>
  new TextEncoder().encode(text).buffer as ArrayBuffer;
const counter = (version: string) => ({
  type: "barracuda/app",
  v: PROTOCOL,
  id: "counter",
  name: "Counter",
  version,
  files: {
    "app.json": utf8(
      JSON.stringify({ format: 1, id: "counter", name: "Counter", version }),
    ),
    "main.lua": utf8(`print("${version}")`),
  },
});

let browser: ReturnType<typeof installBrowser>;
let fake: ReturnType<typeof fakeFiles>;
const realFetch = globalThis.fetch;
beforeEach(() => {
  browser = installBrowser();
  fake = fakeFiles();
  globalThis.fetch = Object.assign(fake.fetch, {
    preconnect: realFetch.preconnect,
  });
});
afterEach(async () => {
  globalThis.fetch = realFetch;
  await browser.close();
});

/** The page as Live Editor opens it: from another window, which hears what happens. */
function opened() {
  const heard: { message: Record<string, unknown>; origin: string }[] = [];
  const opener = {
    postMessage: (message: Record<string, unknown>, origin: string) =>
      heard.push({ message, origin }),
  };
  const toasts: Toast[] = [];
  const context = {
    signal: new AbortController().signal,
    lang: "zh",
    toast: (toast: Toast) => toasts.push(toast),
  } as unknown as PortalContext;
  const slot = browser.document.createElement("div");
  const shown: string[] = [];
  new Incoming(
    context,
    slot,
    (path) => shown.push(path),
    opener as unknown as Window,
  ).start();
  // happy-dom has no MessageEvent: a plain event carries what one would.
  const deliver = (data: unknown, source: unknown = opener) => {
    const event = new browser.window.Event("message");
    Object.defineProperties(event, {
      data: { value: data },
      origin: { value: "https://barracuda.dev" },
      source: { value: source },
    });
    browser.window.dispatchEvent(event);
  };
  const press = async (label: string) => {
    [...slot.querySelectorAll("button")]
      .find((b) => b.textContent === label)!
      .click();
    await settle(30);
  };
  return { heard, deliver, slot, press, shown, toasts };
}

test("only a well-formed app is taken", () => {
  expect(asOffer(counter("0.1.0"))?.files.map((file) => file.name)).toEqual([
    "app.json",
    "main.lua",
  ]);
  expect(asOffer({ ...counter("0.1.0"), id: "../x" })).toBeUndefined();
  expect(
    asOffer({ ...counter("0.1.0"), files: { "../app.json": utf8("{}") } }),
  ).toBeUndefined();
  expect(
    asOffer({ ...counter("0.1.0"), files: { "main.lua": utf8("") } }),
  ).toBeUndefined();
  expect(asOffer({ ...counter("0.1.0"), v: 9 })).toBeUndefined();
});

test("the app is written only once the person installs it, and swapped in whole", async () => {
  const page = opened();
  expect(page.heard[0]).toEqual({
    message: { type: "barracuda/ready", v: 1 },
    origin: "*",
  });
  page.deliver(counter("0.1.0"), {});
  await settle(20);
  expect(page.slot.textContent).toBe("");

  page.deliver(counter("0.1.0"));
  await settle(20);
  expect(page.slot.textContent).toContain("Live Editor 发来一个应用");
  expect(page.slot.textContent).toContain("https://barracuda.dev");
  expect(page.slot.textContent).toContain("这台设备上还没有");
  expect(fake.files.size).toBe(0);

  await page.press("安装到设备");
  expect(
    new TextDecoder().decode(fake.files.get(`${APPS}/counter.bapp/main.lua`)),
  ).toBe('print("0.1.0")');
  expect([...fake.dirs].filter((dir) => dir.startsWith(APPS))).toEqual([
    APPS,
    `${APPS}/counter.bapp`,
  ]);
  expect(
    page.heard.slice(1).map(({ message, origin }) => [message.state, origin]),
  ).toEqual([
    ["review", "https://barracuda.dev"],
    ["writing", "https://barracuda.dev"],
    ["installed", "https://barracuda.dev"],
  ]);
  expect(page.shown).toEqual([`${APPS}/counter.bapp`]);
});

test("a new version replaces the installed one, and nothing is left beside it", async () => {
  const page = opened();
  page.deliver(counter("0.1.0"));
  await settle(20);
  await page.press("安装到设备");
  page.deliver(counter("0.2.0"));
  await settle(20);
  expect(page.slot.textContent).toContain("替换已安装的 0.1.0");
  await page.press("安装到设备");
  expect(
    new TextDecoder().decode(fake.files.get(`${APPS}/counter.bapp/main.lua`)),
  ).toBe('print("0.2.0")');
  expect([...fake.dirs].filter((dir) => dir.startsWith(APPS))).toEqual([
    APPS,
    `${APPS}/counter.bapp`,
  ]);
});

test("refusing changes nothing, and a failed write says why and can be tried again", async () => {
  const page = opened();
  page.deliver(counter("0.1.0"));
  await settle(20);
  await page.press("不安装");
  expect(page.heard.at(-1)?.message.state).toBe("refused");
  expect(fake.dirs.has(APPS)).toBe(false);

  page.deliver(counter("0.1.0"));
  await settle(20);
  const realFake = globalThis.fetch;
  globalThis.fetch = Object.assign(
    async (url: RequestInfo | URL, init?: RequestInit) =>
      String(url).includes("/upload/")
        ? new Response(JSON.stringify({ error: "read_only" }), { status: 403 })
        : realFake(url, init),
    { preconnect: realFetch.preconnect },
  );
  await page.press("安装到设备");
  expect(page.heard.at(-1)?.message.state).toBe("failed");
  expect(page.toasts.length).toBe(1);
  expect(
    [...page.slot.querySelectorAll("button")].map((b) => b.textContent),
  ).toContain("安装到设备");
  expect(fake.dirs.has(`${APPS}/counter.bapp`)).toBe(false);
});
