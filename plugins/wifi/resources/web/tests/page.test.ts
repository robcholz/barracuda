import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  installBrowser,
  settle,
} from "../../../../captive-portal/resources/web/tests/browser";
import type {
  Lang,
  PortalContext,
  Toast,
} from "../../../../captive-portal/resources/web/ui";
import {
  mount,
  nearbyNetworks,
  signalBars,
  type VisibleNetwork,
  type WifiStatus,
} from "../entry";

const CONNECTED: WifiStatus = {
  capabilities: {
    access_point: true,
    scanning: true,
    station_configuration: true,
  },
  station: { state: "connected", ssid: "HomeNet" },
  access_point: { state: "stopped" },
};
const SETUP: WifiStatus = {
  ...CONNECTED,
  station: { state: "disconnected" },
  access_point: { state: "started", ssid: "Barracuda Setup" },
};
const HOST: WifiStatus = {
  capabilities: {
    access_point: false,
    scanning: false,
    station_configuration: false,
  },
  station: { state: "connected" },
  access_point: { state: "stopped" },
};
const SCAN: VisibleNetwork[] = [
  { ssid: "Pixel-7", signal_dbm: -79, secured: true },
  { ssid: "HomeNet", signal_dbm: -48, secured: true },
  { ssid: "", signal_dbm: -40, secured: true },
  { ssid: "Cafe Guest", signal_dbm: -70, secured: false },
  { ssid: "Office-5F", signal_dbm: -61, secured: true },
  { ssid: "Office-5F", signal_dbm: -72, secured: true },
];

type Handler = (init: RequestInit) => Response | Promise<Response>;
const json = (value: unknown, status = 200) =>
  new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });

let browser: ReturnType<typeof installBrowser>;
let calls: { url: string; method: string; body?: string }[];
let routes: Record<string, Handler>;
const realFetch = globalThis.fetch;

beforeEach(() => {
  browser = installBrowser();
  calls = [];
  routes = {
    "GET /api/wifi": () => json(CONNECTED),
    "GET /api/wifi/scan": () => json(SCAN),
    "PUT /api/wifi": () => new Response(null, { status: 204 }),
    "DELETE /api/wifi": () => new Response(null, { status: 204 }),
  };
  globalThis.fetch = Object.assign(
    async (url: RequestInfo | URL, init: RequestInit = {}) => {
      const method = init.method ?? "GET";
      calls.push({
        url: String(url),
        method,
        body: init.body as string | undefined,
      });
      const handler = routes[`${method} ${String(url)}`];
      return handler ? handler(init) : new Response(null, { status: 404 });
    },
    { preconnect: realFetch.preconnect },
  );
});
afterEach(async () => {
  globalThis.fetch = realFetch;
  await browser.close();
});

async function render(lang: Lang = "zh") {
  const controller = new AbortController();
  const toasts: Toast[] = [];
  const refreshes = { count: 0 };
  const context: PortalContext = {
    signal: controller.signal,
    lang,
    toast: (toast) => {
      if (!controller.signal.aborted) toasts.push(toast);
    },
    navigate: () => {},
    status: () => null,
    refreshStatus: async () => {
      refreshes.count++;
    },
  };
  const root = browser.document.createElement("div");
  browser.document.body.append(root);
  const cleanup = (await mount(root, context)) as () => void;
  await settle();
  const text = () => root.textContent ?? "";
  const row = (ssid: string) =>
    [...root.querySelectorAll<HTMLElement>(".bc-trow, .bc-list-row")].find(
      (element) => element.textContent?.includes(ssid),
    )!;
  const form = () => root.querySelector("form")!;
  const input = (name: string) =>
    root.querySelector<HTMLInputElement>(`form [name="${name}"]`)!;
  const submit = async () => {
    form().dispatchEvent(
      new browser.window.Event("submit", { cancelable: true }),
    );
    await settle();
  };
  const errors = () =>
    [...root.querySelectorAll<HTMLElement>(".bc-hint--error")]
      .filter((line) => !line.hidden)
      .map((line) => line.textContent);
  const button = (label: string) =>
    [...root.querySelectorAll<HTMLButtonElement>("button")].find(
      (element) => element.textContent?.trim() === label,
    )!;
  return {
    root,
    toasts,
    refreshes,
    controller,
    cleanup,
    text,
    row,
    form,
    input,
    submit,
    errors,
    button,
  };
}

const writes = () => calls.filter((call) => call.method !== "GET");

test("signal bars and the nearby list follow the design", () => {
  expect([-48, -61, -70, -79, null].map(signalBars)).toEqual([4, 3, 2, 1, 0]);
  expect(nearbyNetworks(SCAN)).toEqual([
    { ssid: "HomeNet", signal_dbm: -48, secured: true },
    { ssid: "Office-5F", signal_dbm: -61, secured: true },
    { ssid: "Cafe Guest", signal_dbm: -70, secured: false },
    { ssid: "Pixel-7", signal_dbm: -79, secured: true },
  ]);
});

test("renders the status and the scan in Chinese", async () => {
  const page = await render("zh");
  expect(calls.map((call) => call.url)).toEqual([
    "/api/wifi",
    "/api/wifi/scan",
  ]);
  const status = page.root.querySelector("dl[role=status]")!;
  expect(status.textContent).toBe("状态已连接网络HomeNet配置热点已关闭");
  expect(status.querySelector(".bc-badge--signal")?.textContent).toBe("已连接");
  expect(page.text()).toContain("扫描、连接或忘记无线网络。");
  expect(page.text()).toContain("附近的网络04");
  const rows = [...page.root.querySelectorAll(".bc-trow")].map(
    (row) => row.textContent,
  );
  expect(rows).toEqual([
    "HomeNet当前加密-48 dBm",
    "Office-5F加密-61 dBm",
    "Cafe Guest开放-70 dBm",
    "Pixel-7加密-79 dBm",
  ]);
  // the current network is not a join target
  expect(page.row("HomeNet").tagName).toBe("DIV");
  expect(page.text()).toContain("设备会断开 HomeNet，并开启配置热点。");
  expect(page.button("忘记网络")).toBeTruthy();
  expect(page.root.querySelector("hl-figure")?.getAttribute("name")).toBe(
    "wifi",
  );
});

test("renders in English", async () => {
  routes["GET /api/wifi"] = () => json(SETUP);
  const page = await render("en");
  const status = page.root.querySelector("dl[role=status]")!;
  expect(status.textContent).toBe(
    "StatusNot connectedNetwork—Setup hotspotBarracuda Setup",
  );
  expect(page.text()).toContain("Nearby networks04");
  expect(page.row("Cafe Guest").textContent).toBe("Cafe GuestOpen-70 dBm");
  expect(page.row("Office-5F").textContent).toBe("Office-5FSecured-61 dBm");
  expect(page.text()).toContain("Enter a network name");
  // nothing to forget while disconnected
  expect(page.text()).not.toContain("Forget");
});

test("the router sweeps while a scan runs", async () => {
  let finish!: (response: Response) => void;
  routes["GET /api/wifi/scan"] = () =>
    new Promise<Response>((resolve) => (finish = resolve));
  const page = await render();
  const figure = page.root.querySelector("hl-figure")!;
  const rescan = page.button("正在扫描…");
  expect(figure.getAttribute("scan")).toBe("true");
  expect(rescan.getAttribute("aria-busy")).toBe("true");
  expect(page.text()).toContain("正在扫描…");
  finish(json(SCAN));
  await settle();
  expect(figure.getAttribute("scan")).toBe("false");
  expect(page.button("重新扫描").getAttribute("aria-busy")).toBe("false");

  routes["GET /api/wifi/scan"] = () => json([]);
  page.button("重新扫描").click();
  await settle();
  expect(calls.filter((call) => call.url === "/api/wifi/scan")).toHaveLength(2);
  expect(page.text()).toContain("没有发现网络");
});

test("joins a secured network after validating the password", async () => {
  const page = await render();
  page.row("Office-5F").click();
  expect(page.row("Office-5F").getAttribute("aria-expanded")).toBe("true");
  expect(page.form().textContent).toContain("密码");
  expect(page.form().textContent).toContain("连接后，在新网络中重新打开门户。");

  await page.submit();
  expect(page.errors()).toEqual(["请填写 密码。"]);
  page.input("password").value = "short";
  await page.submit();
  expect(page.errors()).toEqual(["请输入 8 到 63 个字符的密码。"]);
  expect(writes()).toEqual([]);

  page.input("password").value = "correct horse";
  await page.submit();
  expect(writes()).toEqual([
    {
      url: "/api/wifi",
      method: "PUT",
      body: JSON.stringify({ ssid: "Office-5F", password: "correct horse" }),
    },
  ]);
  expect(page.toasts).toEqual([
    {
      kind: "success",
      title: "已连接 Office-5F",
      body: "连接后，在新网络中重新打开门户。",
      action: undefined,
    },
  ]);
  // the form closes, the password is gone and the status is read again
  expect(page.root.querySelector("form")).toBeNull();
  expect(calls.filter((call) => call.url === "/api/wifi")).toHaveLength(3);
  // and the portal's: the sidebar and the overview follow
  expect(page.refreshes.count).toBe(1);
});

test("joins an open network without a password", async () => {
  const page = await render("en");
  page.row("Cafe Guest").click();
  expect(page.form().textContent).toContain(
    "Password (leave empty for an open network)",
  );
  await page.submit();
  expect(writes()[0]?.body).toBe(
    JSON.stringify({ ssid: "Cafe Guest", password: "" }),
  );
  expect(page.toasts[0]?.title).toBe("Connected to Cafe Guest");
});

test("a rejected join keeps the form and reports the status", async () => {
  routes["PUT /api/wifi"] = () => json({ error: "connection_failed" }, 422);
  const page = await render();
  page.row("Pixel-7").click();
  page.input("password").value = "wrong password";
  await page.submit();
  expect(page.toasts).toEqual([
    { kind: "error", title: "配置被拒绝", code: "422" },
  ]);
  expect(page.input("password").value).toBe("wrong password");
  expect(page.refreshes.count).toBe(0);
});

test("joins a network entered by name", async () => {
  const page = await render("en");
  page.row("Enter a network name").click();
  await page.submit();
  expect(page.errors()).toEqual(["Enter the Network name."]);
  page.input("ssid").value = "x".repeat(33);
  await page.submit();
  expect(page.errors()).toEqual(["Use at most 32 bytes for the network name."]);
  page.input("ssid").value = "Hidden Lab";
  page.input("password").value = "12345678";
  await page.submit();
  expect(writes()[0]?.body).toBe(
    JSON.stringify({ ssid: "Hidden Lab", password: "12345678" }),
  );
});

test("forgets the current network", async () => {
  const page = await render();
  routes["GET /api/wifi"] = () => json(SETUP);
  page.button("忘记网络").click();
  await settle();
  expect(writes()).toEqual([
    { url: "/api/wifi", method: "DELETE", body: undefined },
  ]);
  expect(page.toasts[0]).toMatchObject({
    kind: "success",
    title: "已忘记 HomeNet",
  });
  expect(page.text()).not.toContain("忘记网络");
  expect(page.root.querySelector("dl")?.textContent).toContain(
    "Barracuda Setup",
  );
  expect(page.refreshes.count).toBe(1);
});

test("reports failed reads and writes", async () => {
  routes["GET /api/wifi/scan"] = () => json({ error: "wifi" }, 500);
  routes["DELETE /api/wifi"] = () => json({ error: "storage" }, 500);
  const page = await render("en");
  expect(page.toasts).toEqual([
    { kind: "error", title: "Scan failed", code: "500" },
  ]);
  page.button("Forget network").click();
  await settle();
  expect(page.toasts[1]).toMatchObject({
    kind: "error",
    title: "Submission failed",
    code: "500",
  });

  routes["GET /api/wifi"] = () => new Response(null, { status: 503 });
  const other = await render("zh");
  expect(other.toasts).toEqual([
    { kind: "error", title: "无法读取 Wi-Fi 状态", code: "503" },
  ]);
  expect(other.root.querySelector("dl")?.textContent).toBe(
    "状态—网络—配置热点—",
  );
});

test("a platform-managed network offers nothing to change", async () => {
  routes["GET /api/wifi"] = () => json(HOST);
  const page = await render();
  expect(calls.map((call) => call.url)).toEqual(["/api/wifi"]);
  expect(page.root.querySelector("dl")?.textContent).toBe(
    "状态已连接网络—配置热点—",
  );
  expect(page.text()).toContain("网络由当前平台管理");
  expect(page.text()).not.toContain("手动输入网络名称");
  expect(page.text()).not.toContain("忘记此网络");
  expect(page.button("重新扫描").disabled).toBe(true);
});

test("the phone layout is the design's list", async () => {
  browser.browser.happyDOM.setViewport({ width: 390, height: 844 });
  const page = await render("en");
  expect(page.root.querySelector(".bc-page")).toBeNull();
  expect(
    page.root.querySelector("section[aria-label='Current connection']")
      ?.textContent,
  ).toBe("Wi-FiConnectedHomeNet");
  expect(page.root.querySelectorAll("ul > li")).toHaveLength(5);
  page.row("Office-5F").click();
  expect(page.form().querySelector("input")?.style.fontSize).toBe("16px");
  expect(page.button("Forget HomeNet")).toBeTruthy();
  expect(page.text()).toContain("The device then turns on the setup hotspot.");
});

test("cleans up with the page", async () => {
  let finish!: (response: Response) => void;
  routes["GET /api/wifi/scan"] = () =>
    new Promise<Response>((resolve) => (finish = resolve));
  const page = await render();
  page.controller.abort();
  page.cleanup();
  expect(page.root.childElementCount).toBe(0);
  finish(json({ error: "wifi" }, 500));
  await settle();
  expect(page.toasts).toEqual([]);
});
