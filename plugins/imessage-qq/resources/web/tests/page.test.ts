import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import {
  json,
  pageHarness,
  type FetchCall,
} from "../../../../captive-portal/resources/web/tests/page";
import { mount } from "../entry";

const ENDPOINT = "/api/gateway/qq";
const LOGIN = "/api/gateway/qq/login";
const QR_URL =
  "https://q.qq.com/qqbot/openclaw/connect.html?task_id=ef0f2725&source=barracuda&_wv=2";

let harness: ReturnType<typeof pageHarness>;
/** What `GET /login` answers next; `POST /login` answers `post()`. */
let status: Record<string, unknown>;
let post: () => Response;
const mounted: { unmount(): void }[] = [];

beforeEach(() => {
  jest.useFakeTimers();
  harness = pageHarness();
  status = { status: "idle", configured: false };
  post = () => json(200, { url: QR_URL });
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
/** Submits the form, as 验证并保存 does. */
async function submit(page: Page) {
  page.root
    .querySelector("form")!
    .dispatchEvent(
      new harness.browser.window.Event("submit", { cancelable: true }),
    );
  await flush();
}

test("on mount it starts a binding and shows the code and the steps, without a countdown", async () => {
  const page = await render("zh");
  expect(requests()).toEqual([`GET ${LOGIN}`, `POST ${LOGIN}`]);
  expect(harness.calls[1].body).toEqual({});
  expect(page.query(".bc-header h1")?.textContent).toBe("QQ");
  expect(page.query(".bc-lead")?.textContent).toBe(
    "用手机 QQ 扫码，把设备接入 QQ 消息通道。",
  );
  expect(page.query("hl-figure")?.getAttribute("name")).toBe("riffle");
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("扫码绑定");
  expect(page.text()).toContain("用手机 QQ 扫码，选好机器人并确认");
  // a Card whose body holds the QR plate beside the Steps
  expect(plate(page).className).toBe("bc-qr");
  expect(plate(page).querySelector("svg")?.getAttribute("width")).toBe("168");
  expect(plate(page).parentElement?.className).toBe("bc-card__body");
  expect(page.query(".bc-card__body .bc-steps")).not.toBeNull();
  expect(steps(page)).toEqual(["step", null, null]);
  expect(page.text()).toContain(
    "01用 QQ 扫描二维码02在手机上选好机器人03绑定完成",
  );
  // QQ does not say how long a code lives
  expect(page.text()).not.toContain("过期还剩");
  expect(button(page, "换一张二维码")?.className).toContain(
    "bc-button--outline",
  );
  // App ID and App Secret wait under 高级: no footer until it opens
  expect(page.input("app_id").closest(".bc-row")).not.toBeNull();
  expect(footer(page).hidden).toBe(true);
});

test("polls every 2 s until the binding is done, then shows the bot with a toast and stops", async () => {
  const page = await render("zh");
  status = { status: "wait", configured: false };
  await advance(2000);
  expect(requests().slice(2)).toEqual([`GET ${LOGIN}`]);
  expect(plate(page).textContent).toBe("");
  expect(page.refreshes.count).toBe(0);
  status = { status: "confirmed", configured: true, app_id: "102345678" };
  await advance(2000);
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("绑定");
  expect(page.text()).toContain("这台设备在 QQ 里的机器人");
  const card = page.query(".bc-card[role=status]")!;
  expect(card.querySelector(".bc-option-title")?.textContent).toBe("QQ 已绑定");
  // the bare success check says it: no tile, no badge; the App ID is mono
  expect(card.querySelector("svg.bc-success")).not.toBeNull();
  expect(card.querySelector(".bc-option-icon")).toBeNull();
  expect(card.querySelector(".bc-badge")).toBeNull();
  expect(card.querySelector(".bc-kv dt")?.textContent).toBe("App ID");
  expect(card.querySelector(".bc-kv dd.bc-mono")?.textContent).toBe(
    "102345678",
  );
  expect(page.toasts).toEqual([{ kind: "success", title: "QQ 已绑定" }]);
  expect(page.refreshes.count).toBe(1);
  // linked: the page reads the channel's mode and accounts
  expect(requests().at(-1)).toBe(`GET ${ENDPOINT}/status`);
  const count = harness.calls.length;
  await advance(10_000);
  expect(harness.calls).toHaveLength(count);
  await press(page, "重新绑定");
  expect(requests().at(-1)).toBe(`POST ${LOGIN}`);
  expect(plate(page).querySelector("svg")).not.toBeNull();
});

test("an expired code dims, and 换一张二维码 (now primary) posts again", async () => {
  const page = await render("zh");
  status = { status: "expired", configured: false };
  await advance(2000);
  expect(plate(page).textContent).toBe("二维码已过期");
  expect(plate(page).className).toBe("bc-qr bc-qr--dim");
  expect(steps(page)).toEqual([null, null, null]);
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

test("a failed binding or start says what to do, never the device's internals", async () => {
  post = () =>
    json(502, {
      error: "upstream_unavailable",
      message: "rate limited",
      code: "429",
    });
  const page = await render("zh");
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "设备连不上服务",
    body: "检查设备的网络后重试。",
    code: "502 · 429",
    action: undefined,
  });
  expect(plate(page).textContent).toBe("绑定失败");

  post = () => json(200, { url: QR_URL });
  await press(page, "换一张二维码");
  status = {
    status: "failed",
    configured: false,
    message: "QQ rejected the App Secret (code 10004): 机器人不存在",
  };
  await advance(2000);
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "绑定失败",
    body: "检查设备的网络后，换一张二维码再试。",
  });
  expect(plate(page).textContent).toBe("绑定失败");
});

test("a channel already linked shows its bot and 重新绑定 without starting a binding", async () => {
  status = { status: "idle", configured: true, app_id: "102345678" };
  const page = await render("en");
  expect(requests()).toEqual([`GET ${LOGIN}`, `GET ${ENDPOINT}/status`]);
  expect(page.query(".bc-row__label .bc-title")?.textContent).toBe("Link");
  expect(page.text()).toContain("This device as a bot in QQ");
  expect(page.text()).toContain("QQ linked");
  expect(page.text()).toContain("App ID102345678");
  expect(button(page, "Link again")).toBeDefined();
  page.unmount();
  expect(requests()).toEqual([`GET ${LOGIN}`, `GET ${ENDPOINT}/status`]);
});

test("a running session on mount is restarted for this page; leaving cancels it", async () => {
  status = { status: "wait", configured: true, app_id: "1" };
  const page = await render("zh");
  expect(requests()).toEqual([`GET ${LOGIN}`, `POST ${LOGIN}`]);
  page.unmount();
  const last = harness.calls.at(-1)!;
  expect([last.method, last.url]).toEqual(["DELETE", LOGIN]);
  expect(last.init.keepalive).toBe(true);
  const count = harness.calls.length;
  await advance(10_000);
  expect(harness.calls).toHaveLength(count);
});

test("App ID and App Secret sit under 高级; a save cancels the binding and shows the bot", async () => {
  const page = await render("zh");
  expect(page.text()).toContain("手动填写 App ID 和 App Secret");
  page.query<HTMLButtonElement>(".bc-disclosure")!.click();
  expect(footer(page).hidden).toBe(false);
  expect(footer(page).textContent).toBe("清空验证并保存");
  expect(page.input("app_id").type).toBe("text");
  expect(page.input("app_secret").type).toBe("password");
  expect(page.input("api_base").value).toBe("https://api.sgroup.qq.com");
  expect(page.input("token_url").value).toBe(
    "https://bots.qq.com/app/getAppAccessToken",
  );
  await submit(page);
  expect(requests()).toEqual([`GET ${LOGIN}`, `POST ${LOGIN}`]);
  expect(page.text()).toContain("请填写 App ID。");
  expect(page.text()).toContain("请填写 App Secret。");

  page.type("app_id", "102345678");
  page.type("app_secret", "s3cret");
  await submit(page);
  const save = harness.calls.find((call) => call.url === ENDPOINT)!;
  expect(save.body).toEqual({
    app_id: "102345678",
    app_secret: "s3cret",
    api_base: "https://api.sgroup.qq.com",
    token_url: "https://bots.qq.com/app/getAppAccessToken",
  });
  expect(page.toasts.at(-1)?.title).toBe("设备已接受配置");
  expect(page.input("app_secret").value).toBe("");
  expect(page.refreshes.count).toBe(1);
  // the typed bot replaced the QR session: cancel it, show the bot and read the channel
  expect(requests().slice(-2)).toEqual([
    `DELETE ${LOGIN}`,
    `GET ${ENDPOINT}/status`,
  ]);
  expect(page.text()).toContain("QQ 已绑定");
  expect(page.text()).toContain("App ID102345678");
});

test("QQ's rejection of a typed secret lands on the field with its code", async () => {
  const page = await render("zh");
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) =>
    call.url === ENDPOINT
      ? json(422, {
          error: "verification_failed",
          message: "机器人不存在",
          code: "10004",
        })
      : reply(call);
  page.query<HTMLButtonElement>(".bc-disclosure")!.click();
  page.type("app_id", "102345678");
  page.type("app_secret", "wrong");
  await submit(page);
  const error = page
    .input("app_secret")
    .closest(".bc-field")!
    .querySelector(".bc-hint--error")!;
  // the field says what to do; QQ's own words stay in the toast
  expect(error.textContent).toBe("检查 App ID 和 App Secret。 · 10004");
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "配置被拒绝",
    body: "机器人不存在",
    code: "422",
  });
  // the code is still there to scan
  expect(plate(page).querySelector("svg")).not.toBeNull();
});

test("renders in English", async () => {
  const page = await render("en");
  for (const phrase of [
    "Scan with QQ to connect the device to the QQ channel.",
    "Link by QR",
    "Scan with QQ on your phone, pick a bot and confirm",
    "Scan the code in QQ",
    "Pick a bot on your phone",
    "New code",
    "Enter App ID and App Secret by hand",
  ])
    expect(page.text()).toContain(phrase);
});

test("on a phone it shows the steps and a link to copy instead of the code", async () => {
  harness.browser.browser.happyDOM.setViewport({ width: 390, height: 844 });
  const page = await render("zh");
  expect(requests()).toEqual([`GET ${LOGIN}`]);
  expect(page.query("[data-theme=light]")).toBeNull();
  expect(steps(page)).toEqual(["step", null, null]);
  expect(page.text()).toContain("在电脑或另一台设备上打开此页扫码");
  expect(button(page, "复制链接")).toBeDefined();
});

test("linked: 模式 offers all three modes and 授权账号 lists the person who scanned", async () => {
  status = { status: "idle", configured: true, app_id: "102345678" };
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) => {
    if (call.url === `${ENDPOINT}/status`)
      return json(200, {
        configured: true,
        mode: "send_receive",
        receive: { state: "receiving" },
        owners: { count: 1 },
      });
    if (call.url === `${ENDPOINT}/owners`)
      return json(200, {
        owners: [{ id: "7F3A9B2E41D04C6A8E15B2D93F0A6C71" }],
        pairing: { code: "482913", expires_in: 600 },
        ignored: 0,
      });
    return reply(call);
  };
  const page = await render("zh");
  expect(
    [...page.root.querySelectorAll('[aria-label="模式"] .bc-option-title')].map(
      (node) => node.textContent,
    ),
  ).toEqual(["停用", "仅发送", "收发"]);
  expect(page.text()).toContain("在 QQ 里给机器人发送482913");
  expect(page.text()).toContain("7F3A9B2E41D04C6A8E15B2D93F0A6C71移除");
  // relinking hides the mode and accounts until the channel is linked again
  const modeRow = () =>
    page.query('[aria-label="模式"]')!.closest<HTMLElement>(".bc-row")!;
  expect(modeRow().hidden).toBe(false);
  await press(page, "重新绑定");
  expect(modeRow().hidden).toBe(true);
});

test("with every receive slot taken it says so and names the limit", async () => {
  status = { status: "idle", configured: true, app_id: "102345678" };
  const reply = harness.reply;
  harness.reply = async (call: FetchCall) => {
    if (call.url === `${ENDPOINT}/status`)
      return json(200, {
        configured: true,
        mode: "send_receive",
        receive: { state: "no_slot", capacity: 2 },
        owners: { count: 1 },
      });
    if (call.url === `${ENDPOINT}/owners`)
      return json(200, {
        owners: [{ id: "c2c:7F3A9B2E41D0", label: "QQ 用户" }],
        pairing: { code: "482913", expires_in: 581 },
        ignored: 0,
      });
    return reply(call);
  };
  const page = await render("zh");
  for (const phrase of [
    "等待名额名额",
    "收发名额已满",
    "把其他通道改为仅发送或停用后，QQ 会自动开始接收",
    "QQ 用户c2c:7F3A9B2E41D0移除",
  ])
    expect(page.text()).toContain(phrase);
  // the caution alert sits under the header frame, before the form
  const alert = page.query<HTMLElement>(".bc-header + .bc-alert[role=status]");
  expect(alert?.hidden).toBe(false);
  expect(alert?.nextElementSibling?.classList.contains("bc-form")).toBe(true);
});
