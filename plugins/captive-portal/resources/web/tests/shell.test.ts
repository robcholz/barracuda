import { afterEach, expect, spyOn, test } from "bun:test";
import { installBrowser, settle } from "./browser";
import { Portal, routeOf, STORAGE } from "../src/shell";
import { layoutModules } from "../src/overview";
import type { PortalContext, WebEntry } from "../src/contract";

const text = (zh: string, en = zh) => ({ zh, en });
function record(
  id: string,
  group: WebEntry["group"],
  order: number,
  title: { zh: string; en: string },
  figure: boolean,
): WebEntry {
  return {
    id,
    group,
    order,
    title,
    summary: text(`${title.zh} 摘要`, `${title.en} summary`),
    icon: `/portal/assets/${id}/icon.svg`,
    figure: figure ? `/portal/assets/${id}/figure.js` : null,
    module: `/portal/assets/${id}/entry.js`,
  };
}
/** The nine built-in registrations, deliberately out of order. */
const NINE = [
  record("imessage-inkbox", "channel", 60, text("Inkbox"), false),
  record("imessage-web", "channel", 10, text("Web 聊天", "Web chat"), true),
  record("imessage-telegram", "channel", 20, text("Telegram"), false),
  record("imessage-wechat", "channel", 30, text("微信", "WeChat"), false),
  record("imessage-qq", "channel", 40, text("QQ"), false),
  record("imessage-bluebubble", "channel", 50, text("BlueBubbles"), false),
  record("agent-websearch", "agent", 20, text("网页搜索", "Web search"), true),
  record("agent", "agent", 10, text("模型配置", "Models"), true),
  record("wifi", "device", 10, text("Wi-Fi"), true),
];

interface Harness {
  portal: Portal;
  document: Document;
  window: Window & typeof globalThis;
  records: {
    value: unknown;
    offline: boolean;
    /** The `/portal/status` reply; `undefined` answers 404. */
    status: unknown;
    statusReads: number;
  };
  loads: string[];
  contexts: PortalContext[];
  cleanups: string[];
  modules: Map<string, unknown>;
  figures: { mounted: number; destroyed: number };
  go(hash: string): Promise<void>;
  $(selector: string): HTMLElement | null;
  $$(selector: string): HTMLElement[];
}

let teardown: (() => Promise<void>) | undefined;
afterEach(async () => {
  await teardown?.();
  teardown = undefined;
});

async function start(
  options: {
    hash?: string;
    entries?: unknown;
    storage?: Record<string, string>;
    width?: number;
    status?: unknown;
  } = {},
): Promise<Harness> {
  const installed = installBrowser(
    `http://localhost/portal/${options.hash ?? ""}`,
  );
  const { window, document } = installed;
  if (options.width)
    installed.browser.happyDOM.setViewport({
      width: options.width,
      height: 900,
    });
  for (const [key, value] of Object.entries(options.storage ?? {}))
    window.localStorage.setItem(key, value);
  const records = {
    value: options.entries ?? NINE,
    offline: false,
    status: options.status,
    statusReads: 0,
  };
  const loads: string[] = [];
  const contexts: PortalContext[] = [];
  const cleanups: string[] = [];
  const figures = { mounted: 0, destroyed: 0 };
  const modules = new Map<string, unknown>();
  const moduleFor = (id: string) => ({
    mount(root: HTMLElement, context: PortalContext) {
      contexts.push(context);
      const label = document.createElement("p");
      label.className = "module-text";
      label.textContent = `${id}:${context.lang}`;
      root.append(label);
      return () => cleanups.push(id);
    },
  });
  const figure = {
    name: "router",
    range: [0, 1, 2] as const,
    mount: () => {
      figures.mounted++;
      return { destroy: () => figures.destroyed++ };
    },
  };
  const portal = new Portal({
    window,
    toastMs: 20,
    fetch: async (url) => {
      if (records.offline) throw new Error("offline fixture");
      if (url === "/portal/status") {
        records.statusReads++;
        return records.status === undefined
          ? new Response(null, { status: 404 })
          : Response.json(records.status);
      }
      return Response.json(records.value);
    },
    load: async (url) => {
      loads.push(url);
      if (modules.has(url)) return modules.get(url);
      const match = /^\/portal\/assets\/([a-z0-9-]+)\/(entry|figure)\.js$/.exec(
        url,
      );
      if (!match) throw new Error(`no fixture for ${url}`);
      return match[2] === "figure" ? { figure } : moduleFor(match[1]);
    },
  });
  teardown = async () => {
    portal.destroy();
    await installed.close();
  };
  await settle();
  const $ = (selector: string) => document.querySelector<HTMLElement>(selector);
  return {
    portal,
    document,
    window,
    records,
    loads,
    contexts,
    cleanups,
    modules,
    figures,
    $,
    $$: (selector) => [...document.querySelectorAll<HTMLElement>(selector)],
    async go(hash) {
      window.location.hash = hash;
      window.dispatchEvent(new window.HashChangeEvent("hashchange"));
      await settle();
    },
  };
}

test("routes read #overview or #<id>", () => {
  expect(routeOf("")).toBe("overview");
  expect(routeOf("#")).toBe("overview");
  expect(routeOf("#overview")).toBe("overview");
  expect(routeOf("#wifi")).toBe("wifi");
  expect(routeOf("#imessage-qq")).toBe("imessage-qq");
});

test("module layout: tiles for device, agent and figured channels; rows for the rest", () => {
  const sorted = [...NINE].sort((a, b) => a.order - b.order);
  const layout = layoutModules(sorted);
  expect(layout.tiles.map((entry) => entry.id).sort()).toEqual(
    ["agent", "agent-websearch", "imessage-web", "wifi"].sort(),
  );
  expect(layout.rows).toHaveLength(5);
  expect(layout.span).toBe(2);
  const [wifi, agent, search] = NINE.slice(-3).reverse();
  const qq = NINE[4];
  expect(layoutModules([wifi, agent, search, qq]).span).toBe(3);
  const figured = {
    ...NINE[2],
    figure: "/portal/assets/imessage-telegram/figure.js",
  };
  const five = layoutModules([wifi, agent, search, NINE[1], figured, qq]);
  expect(five.tiles).toHaveLength(5);
  expect(five.span).toBe(3);
  expect(layoutModules([wifi, qq])).toEqual({
    tiles: [wifi],
    rows: [qq],
    span: 2,
  });
});

test("the overview is generated from the manifest: sidebar groups, steps, tiles and channel rows", async () => {
  const h = await start();
  expect(h.$$(".bc-nav-label").map((node) => node.textContent)).toEqual([
    "设备",
    "智能体",
    "消息通道",
  ]);
  expect(h.$$(".bc-nav-item").map((node) => node.getAttribute("href"))).toEqual(
    [
      "#overview",
      "#wifi",
      "#agent",
      "#agent-websearch",
      "#imessage-web",
      "#imessage-telegram",
      "#imessage-wechat",
      "#imessage-qq",
      "#imessage-bluebubble",
      "#imessage-inkbox",
    ],
  );
  expect(h.$(".bc-nav-item")?.getAttribute("aria-current")).toBe("page");
  expect(h.$(".bc-crumb--current")?.textContent).toBe("概览");
  expect(
    h.$(".portal-hero__kv dd:not([data-status-device])")?.textContent,
  ).toBe("9");
  expect(
    h.$$(".portal-steps a").map((node) => node.getAttribute("href")),
  ).toEqual(["#wifi", "#agent", "#imessage-web"]);
  expect(h.$$(".portal-steps a").map((node) => node.textContent)).toEqual([
    "前往 Wi-Fi",
    "前往模型配置",
    "先用 Web 聊天试试",
  ]);
  expect(h.$$(".bc-tile").map((node) => node.getAttribute("href"))).toEqual([
    "#wifi",
    "#agent",
    "#agent-websearch",
    "#imessage-web",
  ]);
  expect(
    h.$$(".bc-tile hl-figure").map((node) => node.getAttribute("name")),
  ).toEqual(["wifi", "agent", "agent-websearch", "imessage-web"]);
  const rows = h.$$(".portal-channel-row");
  expect(rows.map((node) => node.getAttribute("href"))).toEqual([
    "#imessage-telegram",
    "#imessage-wechat",
    "#imessage-qq",
    "#imessage-bluebubble",
    "#imessage-inkbox",
  ]);
  expect(rows[0].getAttribute("data-hl-at")).toBe("161,149");
  expect(h.$(".portal-channels")?.hasAttribute("data-hl-zone")).toBe(true);
  expect(h.$(".portal-channels hl-figure")?.getAttribute("name")).toBe(
    "riffle",
  );
  expect(h.$(".portal-section__head .portal-count")?.textContent).toBe("09");
  expect(h.$(".portal-hero__figure hl-figure")?.getAttribute("name")).toBe(
    "board",
  );
  // contributor figures load lazily from the manifest, once per URL; built-ins never load
  await settle(20);
  const figureLoads = h.loads.filter((url) => url.endsWith("figure.js"));
  expect(figureLoads.sort()).toEqual(
    NINE.filter((entry) => entry.figure)
      .map((entry) => entry.figure!)
      .sort(),
  );
  expect(h.figures.mounted).toBe(4);
  // leaving the overview destroys every live figure
  await h.go("#wifi");
  expect(h.figures.destroyed).toBe(4);
  expect(h.document.querySelectorAll("hl-figure [data-hairline]").length).toBe(
    0,
  );
});

test("manifest text is rendered as text", async () => {
  const hostile = { ...NINE[8], title: text('<img src=x onerror="alert(1)">') };
  const h = await start({ entries: [hostile] });
  expect(h.$(".bc-tile .bc-title")?.textContent).toBe(hostile.title.zh);
  expect(h.document.querySelectorAll("img")).toHaveLength(0);
});

test("routing mounts the selected module, swaps it on navigation and cleans up", async () => {
  const h = await start({ hash: "#wifi" });
  expect(h.loads).toContain("/portal/assets/wifi/entry.js");
  expect(h.$(".portal-module .module-text")?.textContent).toBe("wifi:zh");
  expect(h.$$(".bc-crumb").map((node) => node.textContent)).toEqual([
    "设备",
    "/",
  ]);
  expect(h.$(".bc-crumb--current")?.textContent).toBe("Wi-Fi");
  expect(h.$('.bc-nav-item[href="#wifi"]')?.getAttribute("aria-current")).toBe(
    "page",
  );
  expect(h.document.title).toBe("Wi-Fi · Barracuda");

  h.contexts[0].navigate("agent");
  h.window.dispatchEvent(new h.window.HashChangeEvent("hashchange"));
  await settle();
  expect(h.window.location.hash).toBe("#agent");
  expect(h.contexts[0].signal.aborted).toBe(true);
  expect(h.cleanups).toEqual(["wifi"]);
  expect(h.$(".portal-module .module-text")?.textContent).toBe("agent:zh");

  await h.go("#overview");
  expect(h.cleanups).toEqual(["wifi", "agent"]);
  expect(h.$(".portal-module")).toBeNull();
  expect(h.$(".portal-overview")).not.toBeNull();
});

test("an unknown or vanished entry shows the unavailable state, and comes back", async () => {
  const h = await start({ hash: "#nope" });
  expect(h.$(".portal-status")?.getAttribute("data-status")).toBe(
    "unavailable",
  );
  expect(h.$(".portal-status__title")?.textContent).toBe("nope 模块已停用");

  await h.go("#imessage-qq");
  expect(h.$(".module-text")?.textContent).toBe("imessage-qq:zh");
  h.records.value = NINE.filter((entry) => entry.id !== "imessage-qq");
  await h.portal.refresh();
  expect(h.cleanups).toEqual(["imessage-qq"]);
  expect(h.$(".portal-status__title")?.textContent).toBe("QQ 模块已停用");
  expect(h.$(".portal-status__text .bc-lead")?.textContent).toBe(
    "在设备上启用 QQ 插件后刷新。",
  );
  expect(h.$$(".bc-crumb").map((node) => node.textContent)).toEqual([
    "消息通道",
    "/",
  ]);
  expect(h.$('.bc-nav-item[href="#imessage-qq"]')).toBeNull();

  h.records.value = NINE;
  (
    h.$(".portal-status__actions .bc-button--outline") as HTMLButtonElement
  ).click();
  await settle();
  expect(h.$(".module-text")?.textContent).toBe("imessage-qq:zh");
  (
    h.$(".portal-status__actions .bc-button") as HTMLButtonElement | null
  )?.click();
});

test("a manifest refresh that keeps the entry does not remount it", async () => {
  const h = await start({ hash: "#wifi" });
  h.records.value = NINE.map((entry) =>
    entry.id === "agent" ? { ...entry, order: 99 } : entry,
  );
  await h.portal.refresh();
  expect(h.contexts).toHaveLength(1);
  h.records.value = NINE.map((entry) =>
    entry.id === "wifi"
      ? { ...entry, module: "/portal/assets/wifi/entry-2.js" }
      : entry,
  );
  h.modules.set("/portal/assets/wifi/entry-2.js", {
    mount: (root: HTMLElement) => {
      root.textContent = "v2";
    },
  });
  await h.portal.refresh();
  await settle();
  expect(h.contexts[0].signal.aborted).toBe(true);
  expect(h.$(".portal-module")?.textContent).toBe("v2");
});

test("a language change re-renders the shell, remounts the module and is remembered", async () => {
  const h = await start({ hash: "#agent" });
  (h.$(".portal-lang") as HTMLButtonElement).click();
  expect(h.$(".bc-menu")).not.toBeNull();
  (h.$('.bc-menu-item[lang="en"]') as HTMLButtonElement).click();
  await settle();
  expect(h.$(".bc-menu")).toBeNull();
  expect(h.contexts).toHaveLength(2);
  expect(h.contexts[0].signal.aborted).toBe(true);
  expect(h.contexts[1].lang).toBe("en");
  expect(h.$(".module-text")?.textContent).toBe("agent:en");
  expect(h.window.localStorage.getItem(STORAGE.lang)).toBe("en");
  expect(h.document.documentElement.lang).toBe("en");
  expect(h.$(".bc-crumb")?.textContent).toBe("Agent");
  expect(h.$(".bc-crumb--current")?.textContent).toBe("Models");
  expect(h.$(".bc-nav-label")?.textContent).toBe("Device");
});

test("a stored language and theme apply at start; the theme control switches and persists", async () => {
  const h = await start({
    storage: { [STORAGE.lang]: "en", [STORAGE.theme]: "dark" },
  });
  expect(h.document.documentElement.getAttribute("data-theme")).toBe("dark");
  expect(h.$(".bc-display + .bc-lead, .portal-hero__lead")?.textContent).toBe(
    "Set up this device's network, models and message channels.",
  );
  const buttons = h.$$(".bc-segmented button");
  expect(buttons.map((node) => node.getAttribute("aria-pressed"))).toEqual([
    "false",
    "true",
    "false",
  ]);
  buttons[0].click();
  expect(h.document.documentElement.getAttribute("data-theme")).toBe("light");
  expect(h.window.localStorage.getItem(STORAGE.theme)).toBe("light");
  h.$$(".bc-segmented button")[2].click();
  expect(h.window.localStorage.getItem(STORAGE.theme)).toBe("system");
  expect(h.document.documentElement.getAttribute("data-theme")).toBe("light");
});

test("a failed import shows the failed state; retry loads again", async () => {
  const errors = spyOn(console, "error").mockImplementation(() => {});
  try {
    const h = await start();
    let fail = true;
    // the module object throws while "importing" until the fixture is told to recover
    const failing = {
      get mount() {
        if (fail) throw new Error("import failed");
        return (root: HTMLElement) => {
          root.textContent = "recovered";
        };
      },
    };
    h.modules.set("/portal/assets/wifi/entry.js", failing);
    await h.go("#wifi");
    expect(h.$(".portal-status")?.getAttribute("data-status")).toBe("failed");
    expect(h.$(".portal-status__title")?.textContent).toBe("模块加载失败");
    expect(h.$("main")?.hasAttribute("aria-busy")).toBe(false);
    fail = false;
    const [back, retry] = h.$$(".portal-status__actions button");
    expect(back.textContent).toBe("返回概览");
    retry.click();
    await settle();
    expect(h.$(".portal-module")?.textContent).toBe("recovered");
    expect(errors).toHaveBeenCalled();
  } finally {
    errors.mockRestore();
  }
});

test("module toasts reach the stack: successes time out, errors stay, actions run once", async () => {
  const h = await start({ hash: "#wifi" });
  const context = h.contexts[0];
  let ran = 0;
  context.toast({
    kind: "success",
    title: "设备已接受配置",
    action: { label: "去 Web 聊天试试", run: () => ran++ },
  });
  context.toast({ kind: "error", title: "配置被拒绝", code: "422" });
  context.toast({ kind: "info", title: "note", body: "detail" });
  const toasts = () => h.$$(".bc-toast");
  expect(toasts()).toHaveLength(3);
  expect(toasts()[0].classList.contains("bc-toast--success")).toBe(true);
  expect(toasts()[0].getAttribute("role")).toBe("status");
  expect(toasts()[1].getAttribute("role")).toBe("alert");
  expect(toasts()[1].querySelector(".portal-toast__code")?.textContent).toBe(
    "422",
  );
  expect(
    toasts()[1].querySelector(".bc-toast__close")?.getAttribute("aria-label"),
  ).toBe("关闭");
  (
    toasts()[0].querySelector(".bc-toast__body button") as HTMLButtonElement
  ).click();
  expect(ran).toBe(1);
  expect(toasts()).toHaveLength(2);
  await settle(40);
  expect(
    toasts().map((node) => node.querySelector(".bc-toast__title")?.textContent),
  ).toEqual(["配置被拒绝 422"]);
  (toasts()[0].querySelector(".bc-toast__close") as HTMLButtonElement).click();
  expect(toasts()).toHaveLength(0);
  for (let index = 0; index < 6; index++)
    context.toast({ kind: "error", title: `e${index}` });
  expect(
    toasts().map((node) => node.querySelector(".bc-toast__title")?.textContent),
  ).toEqual(["e2", "e3", "e4", "e5"]);
  await h.go("#overview");
  context.toast({ kind: "error", title: "stale" });
  expect(toasts()).toHaveLength(4);
});

test("no entries shows the empty state; an unreachable manifest says so and recovers", async () => {
  const errors = spyOn(console, "error").mockImplementation(() => {});
  try {
    const h = await start({ entries: [] });
    expect(h.$(".portal-status")?.getAttribute("data-status")).toBe("empty");
    expect(h.$(".portal-status__title")?.textContent).toBe(
      "还没有启用的网页模块",
    );
    expect(h.$$(".bc-nav-item")).toHaveLength(1);
    expect(h.$(".portal-badge--offline")).toBeNull();

    h.records.offline = true;
    await h.portal.refresh();
    expect(h.$(".portal-status")?.getAttribute("data-status")).toBe("manifest");
    expect(h.$(".portal-badge--offline")?.textContent).toBe("连接未就绪");

    h.records.offline = false;
    h.records.value = NINE;
    (h.$(".portal-status__actions .bc-button") as HTMLButtonElement).click();
    await settle();
    expect(h.$(".portal-badge--offline")).toBeNull();
    expect(h.$$(".bc-tile")).toHaveLength(4);
  } finally {
    errors.mockRestore();
  }
});

test("a failed refresh keeps the last entries and the mounted module", async () => {
  const errors = spyOn(console, "error").mockImplementation(() => {});
  try {
    const h = await start({ hash: "#wifi" });
    h.records.offline = true;
    h.document.dispatchEvent(new h.window.Event("visibilitychange"));
    await settle();
    expect(h.$(".portal-badge--offline")).not.toBeNull();
    expect(h.$$(".bc-nav-item")).toHaveLength(10);
    expect(h.$(".module-text")?.textContent).toBe("wifi:zh");
    expect(h.contexts[0].signal.aborted).toBe(false);
  } finally {
    errors.mockRestore();
  }
});

test("Ctrl+B and the trigger collapse the sidebar, and the choice is remembered", async () => {
  const h = await start();
  const app = h.$(".bc-app")!;
  h.document.dispatchEvent(
    new h.window.KeyboardEvent("keydown", { key: "b", ctrlKey: true }),
  );
  expect(app.classList.contains("bc-app--collapsed")).toBe(true);
  expect(h.$(".bc-sidebar")?.classList.contains("bc-sidebar--collapsed")).toBe(
    true,
  );
  expect(h.window.localStorage.getItem(STORAGE.sidebar)).toBe("collapsed");
  const trigger = h.$(".bc-sidebar-trigger") as HTMLButtonElement;
  expect(trigger.getAttribute("aria-label")).toBe("展开侧栏");
  expect(trigger.getAttribute("aria-expanded")).toBe("false");
  trigger.click();
  expect(app.classList.contains("bc-app--collapsed")).toBe(false);
  expect(
    (h.$(".bc-sidebar-trigger") as HTMLElement).getAttribute("aria-label"),
  ).toBe("收起侧栏");
});

test("on a phone the overview is the grouped list, and pages get a back link", async () => {
  const h = await start({ width: 390 });
  expect(h.$(".portal-overview")).toBeNull();
  expect(
    h.$$(".portal-phone-home .bc-list-label").map((node) => node.textContent),
  ).toEqual(["设备", "智能体", "消息通道"]);
  expect(h.$$(".portal-phone-home .bc-list-row")).toHaveLength(9);
  expect(h.$(".portal-topbar--phone .portal-brand-name")?.textContent).toBe(
    "Barracuda",
  );
  (h.$(".portal-phone-lang") as HTMLButtonElement).click();
  expect(h.$(".portal-phone-home .bc-list-label")?.textContent).toBe("Device");
  await h.go("#wifi");
  expect(h.$(".portal-back")?.getAttribute("href")).toBe("#overview");
  expect(h.$(".portal-back")?.textContent).toBe("Overview");
  (h.$(".portal-phone-theme") as HTMLButtonElement).click();
  expect(h.window.localStorage.getItem(STORAGE.theme)).toBe("light");
});

const label = (zh: string, en: string) => ({ zh, en });
/** A device that joined HomeNet, has one model and a Telegram channel. */
const STATUS = {
  entries: {
    wifi: {
      state: "ready",
      label: label("已连接", "Connected"),
      detail: "HomeNet",
    },
    agent: { state: "ready", label: label("已配置", "Configured") },
    "agent-websearch": { state: "off", label: label("未配置", "Not set up") },
    "imessage-telegram": {
      state: "ready",
      label: label("已配置", "Configured"),
    },
    "imessage-wechat": {
      state: "attention",
      label: label("等待扫码", "Waiting for scan"),
    },
    "imessage-qq": { state: "off", label: label("未配置", "Not set up") },
  },
} as const;

test("the status fills the badge, sidebar, header, steps, tiles and channel rows", async () => {
  const h = await start({ status: STATUS });
  const badge = h.$(".portal-topbar--desktop .bc-badge");
  expect(badge?.textContent).toBe("Wi-Fi 已连接");
  expect(badge?.classList.contains("bc-badge--signal")).toBe(true);
  // the phone header keeps to the offline badge
  expect(h.$(".portal-topbar--phone .bc-badge")).toBeNull();
  const wifi = h.$('.bc-nav-item[href="#wifi"]')!;
  expect(wifi.querySelector(".portal-nav-aside")?.textContent).toBe("HomeNet");
  expect(wifi.getAttribute("aria-label")).toBe("Wi-Fi · HomeNet");
  expect(h.$$(".portal-nav-aside")).toHaveLength(1);
  expect(h.$$(".portal-hero__kv > *").map((node) => node.textContent)).toEqual([
    "Wi-Fi",
    "HomeNet",
    "插件页面每个页面由一个插件提供，停用插件后页面随之消失",
    "9",
    "连接",
    "HTTP · 明文仅在可信网络中提交密钥",
  ]);
  const steps = h.$$(".portal-steps > li");
  const visible = (node: HTMLElement | null) =>
    !!node && node.style.display !== "none";
  expect(
    steps.map((step) => visible(step.querySelector(".portal-step__done"))),
  ).toEqual([true, true, true]);
  expect(
    steps.map((step) => visible(step.querySelector(".portal-step__link"))),
  ).toEqual([false, false, false]);
  expect(
    steps.map((step) => step.querySelector(".portal-step__done")?.textContent),
  ).toEqual(["已连接 HomeNet", "模型配置 · 已配置", "Telegram · 已配置"]);
  const aside = (selector: string) => {
    const node = h.$(`${selector} .portal-aside`);
    return visible(node) ? node!.textContent : null;
  };
  expect(aside('.bc-tile[href="#wifi"]')).toBe("HomeNet");
  expect(
    h.$('.bc-tile[href="#wifi"] .portal-aside')?.classList.contains("bc-mono"),
  ).toBe(true);
  expect(aside('.bc-tile[href="#agent"]')).toBeNull();
  expect(
    h.$$(".portal-channel-row").map((row) => {
      const node = row.querySelector<HTMLElement>(".portal-aside");
      return visible(node) ? node!.textContent : "";
    }),
  ).toEqual(["已配置", "等待扫码", "未配置", "", ""]);

  h.portal.setLang("en");
  expect(h.$(".portal-topbar--desktop .bc-badge")?.textContent).toBe(
    "Wi-Fi connected",
  );
  expect(h.$(".portal-steps .portal-step__done")?.textContent).toBe(
    "Connected to HomeNet",
  );
  expect(h.$(".portal-channel-row .portal-aside")?.textContent).toBe(
    "Configured",
  );
});

test("a status change repaints in place: figures stay, and a failed read empties the slots", async () => {
  const h = await start({ status: STATUS });
  await settle(20);
  const mounted = h.figures.mounted;
  const tile = h.$('.bc-tile[href="#wifi"]');
  h.records.status = {
    entries: {
      wifi: {
        state: "attention",
        label: label("配置热点", "Setup hotspot"),
        detail: "Barracuda-1A2B",
      },
    },
  };
  await h.portal.refreshStatus();
  expect(h.$('.bc-tile[href="#wifi"]')).toBe(tile);
  expect(h.figures.mounted).toBe(mounted);
  expect(h.figures.destroyed).toBe(0);
  const badge = h.$(".portal-topbar--desktop .bc-badge");
  expect(badge?.textContent).toBe("Wi-Fi 配置热点");
  expect(badge?.classList.contains("bc-badge--signal")).toBe(false);
  expect(tile?.querySelector(".portal-aside")?.textContent).toBe(
    "Barracuda-1A2B",
  );
  // no ready entry: every step links again
  expect(
    h.$$(".portal-step__link").filter((node) => node.style.display !== "none"),
  ).toHaveLength(3);

  h.records.status = undefined;
  await h.portal.refreshStatus();
  expect(h.$(".portal-topbar--desktop .bc-badge")).toBeNull();
  expect(h.$$(".portal-nav-aside")).toHaveLength(0);
  expect(
    h.$$("[data-status-device]").every((node) => node.style.display === "none"),
  ).toBe(true);
  expect(
    h.$$(".portal-aside").every((node) => node.style.display === "none"),
  ).toBe(true);
});

test("the status is read with the manifest, on navigation, on focus and when a page asks", async () => {
  const h = await start({ status: STATUS });
  const reads = h.records.statusReads;
  expect(reads).toBeGreaterThanOrEqual(1);
  await h.go("#wifi");
  await settle();
  const moved = h.records.statusReads;
  expect(moved).toBeGreaterThan(reads);
  expect(h.contexts[0].status()).toEqual(STATUS.entries.wifi);
  expect(h.contexts[0].status("imessage-qq")).toEqual(
    STATUS.entries["imessage-qq"],
  );
  expect(h.contexts[0].status("imessage-inkbox")).toBeNull();
  // a page that saved: the sidebar follows without remounting the page
  h.records.status = {
    entries: {
      wifi: {
        state: "ready",
        label: label("已连接", "Connected"),
        detail: "Office-5F",
      },
    },
  };
  await h.contexts[0].refreshStatus();
  expect(h.records.statusReads).toBe(moved + 1);
  expect(h.$(".portal-nav-aside")?.textContent).toBe("Office-5F");
  expect(h.cleanups).toEqual([]);
  h.window.dispatchEvent(new h.window.Event("focus"));
  await settle();
  expect(h.records.statusReads).toBe(moved + 2);
});

test("on a phone the list shows the detail and the channel labels", async () => {
  const h = await start({ width: 390, status: STATUS });
  const asides = h.$$(".portal-phone-home .bc-list-row").map((row) => {
    const node = row.querySelector<HTMLElement>(".portal-aside");
    return node && node.style.display !== "none" ? node.textContent : "";
  });
  expect(asides).toEqual([
    "HomeNet",
    "",
    "",
    "",
    "已配置",
    "等待扫码",
    "未配置",
    "",
    "",
  ]);
});

test("step 03 depends only on the channels that report a status", async () => {
  const h = await start({
    status: {
      entries: {
        wifi: STATUS.entries.wifi,
        "imessage-telegram": {
          state: "off",
          label: label("未配置", "Not set up"),
        },
      },
    },
  });
  const step = h.$('.portal-steps > li[data-step="channel"]')!;
  expect(
    step.querySelector<HTMLElement>(".portal-step__done")?.style.display,
  ).toBe("none");
  expect(
    step.querySelector<HTMLElement>(".portal-step__link")?.style.display,
  ).toBe("");
  expect(step.querySelector(".portal-step__link")?.textContent).toBe(
    "先用 Web 聊天试试",
  );
});
