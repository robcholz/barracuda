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
  // a Card whose body holds the QR plate beside the Steps
  expect(plate(page).className).toBe("bc-qr");
  expect(plate(page).parentElement?.className).toBe("bc-card__body");
  expect(plate(page).parentElement?.parentElement?.className).toBe("bc-card");
  expect(page.query(".bc-card__body .bc-steps")).not.toBeNull();
  expect(steps(page)).toEqual(["step", null, null]);
  // step numbers are mono and two digits
  expect(page.text()).toContain("01用微信扫描二维码02在手机上确认03绑定完成");
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
  expect(footer(page).hidden).toBe(true);
  page.query<HTMLButtonElement>(".bc-disclosure")!.click();
  expect(footer(page).hidden).toBe(false);
  expect(footer(page).textContent).toBe("清空用 Token 保存");
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
  expect(page.refreshes.count).toBe(1);
  // the hand-entered token replaced the QR session: cancel it, show the link and read the channel
  expect(requests().slice(-2)).toEqual([
    `DELETE ${LOGIN}`,
    `GET /api/gateway/wechat/status`,
  ]);
  expect(page.text()).toContain("微信已绑定");
  page.query<HTMLButtonElement>(".bc-disclosure")!.click();
  expect(footer(page).hidden).toBe(true);
});

test("polls every 2 s: scanned, then linked with a toast, then stops", async () => {
  const page = await render("zh");
  status = { status: "wait", configured: false };
  await advance(2000);
  expect(requests().slice(2)).toEqual([`GET ${LOGIN}`]);
  status = { status: "scanned", configured: false };
  await advance(2000);
  expect(plate(page).textContent).toBe("已扫码");
  expect(plate(page).className).toBe("bc-qr bc-qr--dim");
  expect(plate(page).querySelector(".bc-qr__overlay")?.textContent).toBe(
    "已扫码",
  );
  expect(page.refreshes.count).toBe(0);
  expect(steps(page)).toEqual([null, "step", null]);
  expect(page.query("ol li svg")).not.toBeNull();
  expect(page.query("ol li")?.className).toBe("bc-step bc-step--done");
  status = { status: "confirmed", configured: true };
  await advance(2000);
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("绑定");
  expect(page.text()).toContain("这台设备在微信里的 ClawBot");
  const card = page.query(".bc-card[role=status]")!;
  expect(card.textContent).toContain("微信已绑定");
  expect(
    card.querySelector(".bc-badge:not(.bc-badge--signal)")?.textContent,
  ).toBe("已绑定");
  expect(page.toasts).toEqual([{ kind: "success", title: "微信已绑定" }]);
  // linked: the portal reads the channel's status again
  expect(page.refreshes.count).toBe(1);
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

test("a failed session or start says what to do, never the device's internals", async () => {
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
    title: "设备连不上服务",
    body: "检查设备的网络后重试。",
    code: "502 · -1",
    action: undefined,
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
    body: "检查设备的网络后，换一张二维码再试。",
  });
  expect(plate(page).textContent).toBe("绑定失败");
});

test("a channel already linked shows 重新绑定 without starting a login", async () => {
  status = { status: "idle", configured: true };
  const page = await render("en");
  expect(requests()).toEqual([
    `GET ${LOGIN}`,
    `GET /api/gateway/wechat/status`,
  ]);
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("Link");
  expect(page.text()).toContain("WeChat linked");
  expect(button(page, "Link again")).toBeDefined();
  page.unmount();
  expect(requests()).toEqual([
    `GET ${LOGIN}`,
    `GET /api/gateway/wechat/status`,
  ]);
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

test("linked: 模式 offers 停用 and 收发 only, and 授权账号 shows the bare code", async () => {
  status = { status: "idle", configured: true };
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) => {
    if (call.url === "/api/gateway/wechat/status")
      return json(200, {
        configured: true,
        mode: "send_receive",
        receive: { state: "receiving" },
        owners: { count: 0 },
      });
    if (call.url === "/api/gateway/wechat/owners")
      return json(200, {
        owners: [],
        pairing: { code: "482913", expires_in: 600 },
        ignored: 0,
      });
    return reply(call);
  };
  const page = await render("zh");
  expect(requests()).toEqual([
    `GET ${LOGIN}`,
    "GET /api/gateway/wechat/status",
    "GET /api/gateway/wechat/owners",
  ]);
  expect(
    [...page.root.querySelectorAll('[aria-label="模式"] .bc-option-title')].map(
      (node) => node.textContent,
    ),
  ).toEqual(["停用", "收发"]);
  expect(
    page.query("[role=radiogroup] + [role=status] .bc-badge--signal")
      ?.textContent,
  ).toBe("收发中");
  expect(page.text()).toContain("用微信给 ClawBot 发送482913");
  expect(page.text()).toContain("还没有授权账号");
  // the badge stays live while receiving
  await advance(5_000);
  expect(requests().at(-1)).toBe("GET /api/gateway/wechat/status");

  page.unmount();
  const en = await render("en");
  expect(en.text()).toContain("In WeChat, send ClawBot482913");
  expect(en.text()).toContain("No allowed accounts yet");
});

test("relinking hides the mode and accounts until the channel is linked again", async () => {
  status = { status: "idle", configured: true };
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) =>
    call.url === "/api/gateway/wechat/status"
      ? json(200, {
          configured: true,
          mode: "send_receive",
          owners: { count: 0 },
        })
      : reply(call);
  const page = await render("zh");
  const modeRow = () =>
    page.query('[aria-label="模式"]')!.closest<HTMLElement>(".bc-row")!;
  expect(modeRow().hidden).toBe(false);
  await press(page, "重新绑定");
  expect(modeRow().hidden).toBe(true);
});

test("an expired bot session offers 重新绑定, which starts a QR login", async () => {
  status = { status: "idle", configured: true };
  let receive: Record<string, unknown> = {
    state: "error",
    message: "需要重新扫码 / Scan again to relink",
  };
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) =>
    call.url === "/api/gateway/wechat/status" && call.method === "GET"
      ? json(200, {
          configured: true,
          mode: "send_receive",
          receive,
          owners: { count: 1 },
        })
      : reply(call);
  const page = await render("zh");
  const alert = page.query(".bc-alert--error[role=status]")!;
  expect(alert.textContent).toBe("微信登录已失效重新绑定");
  // the Alert card's markup: a bare icon (the error rule colours it) and the body
  expect(alert.querySelector("svg")?.getAttribute("style")).toBeNull();
  expect(alert.lastElementChild?.classList.contains("bc-alert__body")).toBe(
    true,
  );
  expect(page.text()).not.toContain("微信已绑定");
  // the action is primary: it is the one thing to do
  expect(button(page, "重新绑定")!.className).toBe("bc-button bc-button--sm");

  // the device recovers on its own (a login elsewhere): the linked card comes back
  receive = { state: "receiving" };
  await advance(5_000);
  expect(page.query(".bc-alert--error")).toBeNull();
  expect(page.text()).toContain("微信已绑定");
  expect(button(page, "重新绑定")!.className).toContain("bc-button--outline");

  receive = { state: "error", message: "需要重新扫码 / Scan again to relink" };
  await advance(5_000);
  await press(page, "重新绑定");
  expect(requests().at(-1)).toBe(`POST ${LOGIN}`);
  expect(plate(page).querySelector("svg")).not.toBeNull();
  expect(page.query(".bc-alert--error")).toBeNull();
});

test("another receive error keeps the linked card", async () => {
  status = { status: "idle", configured: true };
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) =>
    call.url === "/api/gateway/wechat/status"
      ? json(200, {
          configured: true,
          mode: "send_receive",
          receive: { state: "error", message: "iLink answered HTTP 502" },
          owners: { count: 1 },
        })
      : reply(call);
  const page = await render("en");
  expect(page.query(".bc-alert--error")).toBeNull();
  expect(page.text()).toContain("WeChat linked");
  expect(page.text()).toContain("iLink answered HTTP 502");
});

test("renders the expired session in English", async () => {
  status = { status: "idle", configured: true };
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) =>
    call.url === "/api/gateway/wechat/status"
      ? json(200, {
          configured: true,
          mode: "send_receive",
          receive: {
            state: "error",
            message: "需要重新扫码 / Scan again to relink",
          },
          owners: { count: 1 },
        })
      : reply(call);
  const page = await render("en");
  expect(page.query(".bc-alert--error")?.textContent).toBe(
    "WeChat login expiredLink again",
  );
});
