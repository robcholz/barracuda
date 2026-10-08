import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import {
  json,
  pageHarness,
  type FetchCall,
} from "../../../../captive-portal/resources/web/tests/page";
import { mount } from "../entry";

const LOGIN = "/api/gateway/wechat/login";
const QR_URL =
  "https://liteapp.weixin.qq.com/q/7GiQu1?qrcode=87fdd86b26d8f56abdeeb0e2cbe81675&bot_type=3";

let harness: ReturnType<typeof pageHarness>;
/** What `GET /login` answers next; `POST /login` always starts a session. */
let status: Record<string, unknown>;
let post: () => Response;
const mounted: { unmount(): void }[] = [];

beforeEach(() => {
  jest.useFakeTimers();
  harness = pageHarness();
  status = { status: "idle", configured: false };
  post = () => json(200, { url: QR_URL, expires_in: 480 });
  harness.reply = async (call: FetchCall) => {
    if (call.url !== LOGIN) return new Response(null, { status: 204 });
    if (call.method === "GET") return json(200, status);
    if (call.method === "POST") return post();
    return new Response(null, { status: 204 });
  };
});
afterEach(async () => {
  for (const page of mounted.splice(0)) page.unmount();
  jest.useRealTimers();
  await harness.close();
});

/** Lets pending fetches and their `.json()` resolve. */
async function flush() {
  for (let i = 0; i < 40; i++) await Promise.resolve();
}
async function advance(ms: number) {
  jest.advanceTimersByTime(ms);
  await flush();
}
/** Clicks the button labelled `label` (no `settle`: `Bun.sleep` never ends under fake timers). */
async function press(page: Page, label: string) {
  button(page, label)!.click();
  await flush();
}
async function render(lang: "zh" | "en" = "zh") {
  const page = await harness.render(mount, lang);
  mounted.push(page);
  await flush();
  return page;
}
type Page = Awaited<ReturnType<typeof render>>;

const requests = () =>
  harness.calls.map((call) => `${call.method} ${call.url}`);
const plate = (page: Page) => page.query("[data-theme=light]")!;
const steps = (page: Page) =>
  [...page.root.querySelectorAll("ol li")].map((item) =>
    item.getAttribute("aria-current"),
  );
const footer = (page: Page) => page.query(".bc-form__footer")!;
const button = (page: Page, label: string) =>
  [...page.root.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => node.textContent === label,
  );

test("on mount it starts a login and shows the code, the steps and the expiry", async () => {
  const page = await render("zh");
  expect(requests()).toEqual([`GET ${LOGIN}`, `POST ${LOGIN}`]);
  expect(harness.calls[1].body).toEqual({});
  expect(page.query(".bc-header h1")?.textContent).toBe("微信");
  expect(page.query(".bc-lead")?.textContent).toBe(
    "用微信扫码，把设备接入微信消息通道。",
  );
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("扫码绑定");
  expect(page.text()).toContain("用手机微信扫码并确认");
  expect(plate(page).querySelector("svg")?.getAttribute("width")).toBe("168");
  expect(plate(page).textContent).toBe("");
  expect(steps(page)).toEqual(["step", null, null]);
  expect(page.text()).toContain("1用微信扫描二维码2在手机上确认3绑定完成");
  expect(page.text()).toContain("二维码过期还剩 8:00");
  expect(button(page, "换一张二维码")?.className).toContain(
    "bc-button--outline",
  );
  await advance(1000);
  expect(page.text()).toContain("二维码过期还剩 7:59");
});

test("the manual token sits under 高级, and the footer shows only while it is open", async () => {
  const page = await render("zh");
  expect(page.text()).toContain("手动填写 Token");
  expect(footer(page).style.display).toBe("none");
  page.query<HTMLButtonElement>(".bc-disclosure")!.click();
  expect(footer(page).style.display).toBe("");
  expect(footer(page).textContent).toContain("POST /api/gateway/wechat");
  expect(button(page, "用 Token 保存")).toBeDefined();
  page.type("token", "tok");
  page.root
    .querySelector("form")!
    .dispatchEvent(
      new harness.browser.window.Event("submit", { cancelable: true }),
    );
  await flush();
  const save = harness.calls.find(
    (call) => call.url === "/api/gateway/wechat",
  )!;
  expect(save.body).toEqual({
    token: "tok",
    api_base: "https://ilinkai.weixin.qq.com",
    app_id: "bot",
    client_version: "131329",
    x_wechat_uin: "MA==",
  });
  expect(page.toasts.at(-1)?.title).toBe("设备已接受配置");
  // the hand-entered token replaced the QR session: cancel it and show the link
  expect(requests().at(-1)).toBe(`DELETE ${LOGIN}`);
  expect(page.text()).toContain("微信已绑定");
  page.query<HTMLButtonElement>(".bc-disclosure")!.click();
  expect(footer(page).style.display).toBe("none");
});

test("polls every 2 s: scanned, then linked with a toast, then stops", async () => {
  const page = await render("zh");
  status = { status: "wait", configured: false };
  await advance(2000);
  expect(requests().slice(2)).toEqual([`GET ${LOGIN}`]);
  status = { status: "scanned", configured: false };
  await advance(2000);
  expect(plate(page).textContent).toBe("已扫码");
  expect(steps(page)).toEqual([null, "step", null]);
  expect(page.query("ol li svg")).not.toBeNull();
  status = { status: "confirmed", configured: true };
  await advance(2000);
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("绑定");
  expect(page.text()).toContain("这台设备在微信里的 ClawBot");
  const card = page.query(".bc-frame[role=status]")!;
  expect(card.textContent).toContain("微信已绑定");
  expect(card.querySelector(".bc-badge--signal")?.textContent).toBe("已绑定");
  expect(page.toasts).toEqual([{ kind: "success", title: "微信已绑定" }]);
  const count = harness.calls.length;
  await advance(10_000);
  expect(harness.calls).toHaveLength(count);
  // 重新绑定 starts a new login
  await press(page, "重新绑定");
  await flush();
  expect(requests().at(-1)).toBe(`POST ${LOGIN}`);
  expect(plate(page).querySelector("svg")).not.toBeNull();
});

test("an expired code dims, and 换一张二维码 (now primary) posts again", async () => {
  const page = await render("zh");
  status = { status: "expired", configured: false };
  await advance(2000);
  expect(plate(page).textContent).toBe("二维码已过期");
  expect(steps(page)).toEqual([null, null, null]);
  expect(page.text()).not.toContain("二维码过期还剩");
  const renew = button(page, "换一张二维码")!;
  expect(renew.className).not.toContain("bc-button--outline");
  const count = harness.calls.length;
  await advance(10_000);
  expect(harness.calls).toHaveLength(count);
  renew.click();
  await flush();
  expect(requests().at(-1)).toBe(`POST ${LOGIN}`);
  expect(plate(page).textContent).toBe("");
});

test("the countdown runs out on its own", async () => {
  const page = await render("zh");
  status = { status: "wait", configured: false };
  post = () => json(200, { url: QR_URL, expires_in: 3 });
  button(page, "换一张二维码")!.click();
  await flush();
  await advance(3000);
  expect(plate(page).textContent).toBe("二维码已过期");
});

test("a failed session or start is reported with the device's words", async () => {
  status = { status: "idle", configured: false };
  post = () =>
    json(502, {
      error: "upstream_unavailable",
      message: "iLink timeout",
      code: "-1",
    });
  const page = await render("zh");
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "提交失败",
    body: "iLink timeout",
    code: "502 · -1",
  });
  expect(plate(page).textContent).toBe("绑定失败");

  post = () => json(200, { url: QR_URL, expires_in: 480 });
  await press(page, "换一张二维码");
  await flush();
  status = { status: "failed", configured: false, message: "store failed" };
  await advance(2000);
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "绑定失败",
    body: "store failed",
  });
  expect(plate(page).textContent).toBe("绑定失败");
});

test("a channel already linked shows 重新绑定 without starting a login", async () => {
  status = { status: "idle", configured: true };
  const page = await render("en");
  expect(requests()).toEqual([`GET ${LOGIN}`]);
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("Link");
  expect(page.text()).toContain("WeChat linked");
  expect(button(page, "Link again")).toBeDefined();
  page.unmount();
  expect(requests()).toEqual([`GET ${LOGIN}`]);
});

test("a running session on mount is restarted for this page", async () => {
  status = { status: "wait", configured: true };
  await render("zh");
  expect(requests()).toEqual([`GET ${LOGIN}`, `POST ${LOGIN}`]);
});

test("leaving the page cancels the session with a keepalive DELETE", async () => {
  const page = await render("zh");
  page.unmount();
  const last = harness.calls.at(-1)!;
  expect([last.method, last.url]).toEqual(["DELETE", LOGIN]);
  expect(last.init.keepalive).toBe(true);
  const count = harness.calls.length;
  await advance(10_000);
  expect(harness.calls).toHaveLength(count);
});

test("renders in English", async () => {
  const page = await render("en");
  for (const phrase of [
    "WeChat",
    "Scan with WeChat to connect the device to the WeChat channel.",
    "Link by QR",
    "Scan the code in WeChat",
    "Code expires in 8:00",
    "New code",
    "Enter a token by hand",
  ])
    expect(page.text()).toContain(phrase);
});

test("on a phone it shows the steps and a link to copy instead of the code", async () => {
  harness.browser.browser.happyDOM.setViewport({ width: 390, height: 844 });
  const written: string[] = [];
  const navigator = globalThis.navigator as Navigator & { clipboard?: unknown };
  const before = Object.getOwnPropertyDescriptor(navigator, "clipboard");
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: async (text: string) => void written.push(text) },
  });
  try {
    const page = await render("zh");
    expect(requests()).toEqual([`GET ${LOGIN}`]);
    expect(page.query("[data-theme=light]")).toBeNull();
    expect(steps(page)).toEqual(["step", null, null]);
    expect(page.text()).toContain("在电脑或另一台设备上打开此页扫码");
    await press(page, "复制链接");
    await flush();
    expect(written).toEqual(["http://localhost/portal/"]);
    expect(page.toasts.at(-1)).toEqual({
      kind: "success",
      title: "已复制链接",
    });
  } finally {
    if (before) Object.defineProperty(navigator, "clipboard", before);
    else delete (navigator as { clipboard?: unknown }).clipboard;
  }
});
