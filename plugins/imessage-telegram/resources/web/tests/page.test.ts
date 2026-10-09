import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  json,
  pageHarness,
  settle,
} from "../../../../captive-portal/resources/web/tests/page";
import { mount } from "../entry";

let harness: ReturnType<typeof pageHarness>;
beforeEach(() => {
  harness = pageHarness();
});
afterEach(async () => {
  await harness.close();
});

/** Renders the page; the mount's `GET` of the channel state (204 here: no state) is checked and set aside. */
async function render(lang: "zh" | "en") {
  const page = await harness.render(mount, lang);
  expect(harness.calls.map((call) => [call.method, call.url])).toEqual([
    ["GET", "/api/gateway/telegram"],
  ]);
  harness.calls.length = 0;
  await settle();
  return page;
}

const GET_ME = {
  ok: true,
  result: {
    id: 1,
    is_bot: true,
    first_name: "Barracuda Home",
    username: "barracuda_home_bot",
  },
};

const texts = (root: ParentNode, selector: string) =>
  [...root.querySelectorAll(selector)].map((node) => node.textContent);

test("renders the design in Chinese: brand title, riffle, Bot row with link, verify, open fold", async () => {
  const page = await render("zh");
  const { root } = page;
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("Telegram");
  expect(root.querySelector(".bc-header h1 .bc-asset-icon")).not.toBeNull();
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "通过 Telegram Bot 收发消息。",
  );
  expect(root.querySelector("hl-figure")?.getAttribute("name")).toBe("riffle");
  expect(
    [...root.querySelectorAll<HTMLElement>(".bc-form > .bc-row")]
      .filter((row) => !row.hidden)
      .map((row) => row.querySelector(".bc-title")?.textContent),
  ).toEqual(["机器人", undefined]);
  expect(root.querySelector(".bc-row__label .bc-muted")?.textContent).toBe(
    "在 @BotFather 发送 /newbot 获得 Token",
  );
  // the handle and the command are machine values
  expect(
    [...root.querySelectorAll(".bc-row__label .bc-muted .bc-mono")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["@BotFather", "/newbot"]);
  const link = root.querySelector<HTMLAnchorElement>(".bc-row__label a")!;
  expect([link.textContent, link.getAttribute("href")]).toEqual([
    "打开 @BotFather",
    "https://t.me/BotFather",
  ]);
  expect(texts(page.input("token").parentElement!, "button")).toEqual([
    "显示",
    "验证",
  ]);
  expect(page.input("api_base").value).toBe("https://api.telegram.org");
  expect(page.input("draft_min_delta_bytes").value).toBe("24");
  expect(root.textContent).not.toContain("/api/");
  expect(texts(root, ".bc-form__footer button")).toEqual([
    "清空",
    "保存并替换通道",
  ]);
  expect(harness.calls).toHaveLength(0);
});

test("renders in English", async () => {
  const page = await render("en");
  const text = page.text();
  for (const phrase of [
    "Send and receive messages through a Telegram bot.",
    "Send /newbot to @BotFather to get a token",
    "Open @BotFather",
    "Verify",
    "Draft minimum delta",
    "Save and replace channel",
  ])
    expect(text).toContain(phrase);
});

test("验证 calls getMe from the browser and shows the bot and a chat link", async () => {
  const page = await render("zh");
  await page.click("验证");
  expect(harness.calls).toHaveLength(0);
  expect(page.text()).toContain("请填写 Bot Token。");

  harness.reply = async () => json(200, GET_ME);
  page.type("token", " 123:abc ");
  await page.click("验证");
  expect(harness.calls.map((call) => call.url)).toEqual([
    "https://api.telegram.org/bot123:abc/getMe",
  ]);
  const card = page.query(".bc-card[role=status]")!;
  expect(card.querySelector(".bc-option-icon")?.textContent).toBe("B");
  expect(card.querySelector(".bc-option-title")?.textContent).toBe(
    "Barracuda Home",
  );
  expect(card.textContent).toContain("@barracuda_home_bot");
  expect(
    card.querySelector(".bc-badge:not(.bc-badge--signal)")?.textContent,
  ).toBe("已验证");
  const chat = [...page.root.querySelectorAll<HTMLElement>(".bc-row")].find(
    (row) => row.textContent?.startsWith("开始对话"),
  )!;
  expect(chat.hidden).toBe(false);
  expect(chat.textContent).toContain("用手机扫码，打开和 Bot 的对话");
  const open = chat.querySelector("a")!;
  expect([open.textContent, open.getAttribute("href")]).toEqual([
    "在 Telegram 中打开",
    "https://t.me/barracuda_home_bot",
  ]);
  expect(chat.querySelector("[data-theme=light] svg path")).not.toBeNull();
  // the link QR is a Card with the plate in its body
  expect(
    chat.querySelector(".bc-card > .bc-card__body > .bc-qr > svg"),
  ).not.toBeNull();

  // editing the token drops the stale result
  page.type("token", "123:abd");
  expect(page.query(".bc-card[role=status]")).toBeNull();
  expect(chat.hidden).toBe(true);
});

test("Telegram's refusal is shown on the token field; the 高级 API base is used", async () => {
  const page = await render("zh");
  harness.reply = async () =>
    json(401, { ok: false, error_code: 401, description: "Unauthorized" });
  page.type("token", "bad");
  page.type("api_base", "https://tg.example.com/");
  await page.click("验证");
  expect(harness.calls[0].url).toBe("https://tg.example.com/botbad/getMe");
  const error = page
    .input("token")
    .closest(".bc-field")!
    .querySelector(".bc-hint--error")!;
  // the field says what to do, then Telegram's status in mono
  expect(error.textContent).toBe(
    "检查 Bot Token 后重新验证。 · 401 Unauthorized",
  );
  expect(error.querySelector(".bc-mono")?.textContent).toBe("401 Unauthorized");
  expect(page.input("token").getAttribute("aria-invalid")).toBe("true");
});

test("no route to Telegram shows a note and saving still works", async () => {
  const page = await render("en");
  harness.reply = async (call) => {
    if (call.url.startsWith("https://api.telegram.org"))
      throw new TypeError("Failed to fetch");
    return new Response(null, { status: 204 });
  };
  page.type("token", "123:abc");
  await page.click("Verify");
  expect(page.text()).toContain(
    "Couldn't reach Telegram; the token is not verified",
  );
  await page.submit();
  // the accepted save reads the channel again for its mode and accounts
  expect(harness.calls.slice(-2).map((call) => call.method)).toEqual([
    "POST",
    "GET",
  ]);
  const save = harness.calls.at(-2)!;
  expect([save.url, save.method]).toEqual(["/api/gateway/telegram", "POST"]);
  expect(save.body).toEqual({
    token: "123:abc",
    api_base: "https://api.telegram.org",
    draft_min_delta_bytes: 24,
  });
  expect(page.toasts.at(-1)?.title).toBe(
    "The device accepted the configuration",
  );
  page.toasts.at(-1)?.action?.run();
  expect(page.routes).toEqual(["imessage-web"]);
  expect(page.input("token").value).toBe("");
});

test("a rejected save toasts the device's message", async () => {
  const page = await render("zh");
  harness.reply = async () =>
    json(422, { error: "registration_failed", message: "gateway refused" });
  page.type("token", "123:abc");
  await page.submit();
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "配置被拒绝",
    body: "gateway refused",
    code: "422",
  });
  expect(page.input("token").value).toBe("123:abc");
  page.unmount();
  expect(page.root.childElementCount).toBe(0);
});

test("a configured channel shows above the form; a save shows it and refreshes the status", async () => {
  harness.reply = async () => json(200, { configured: true });
  const shown = await harness.render(mount, "en");
  await settle();
  const current = shown.query(".bc-form > .bc-row")!;
  expect(current.hidden).toBe(false);
  expect(current.querySelector(".bc-row__label")?.textContent).toBe("Channel");
  expect(current.querySelector(".bc-option-title")?.textContent).toBe(
    "Telegram",
  );
  expect(
    current.querySelector(".bc-badge:not(.bc-badge--signal)")?.textContent,
  ).toBe("Configured");
  shown.unmount();

  harness.reply = async () => json(200, { configured: false });
  const page = await harness.render(mount, "zh");
  await settle();
  const row = page.query(".bc-form > .bc-row")!;
  expect(row.hidden).toBe(true);
  page.type("token", "123:abc");
  harness.reply = async () => json(422, { error: "registration_failed" });
  await page.submit();
  expect(page.refreshes.count).toBe(0);
  expect(row.hidden).toBe(true);
  harness.reply = async () => new Response(null, { status: 204 });
  await page.submit();
  expect(page.refreshes.count).toBe(1);
  expect(row.hidden).toBe(false);
  expect(
    row.querySelector(".bc-badge:not(.bc-badge--signal)")?.textContent,
  ).toBe("已配置");
});

/** The device's channel state and allowed accounts, as a receiving Telegram channel answers them. */
function receiving(mode = "send_receive") {
  harness.reply = async (call) => {
    if (call.method === "POST") return new Response(null, { status: 204 });
    if (call.url === "/api/gateway/telegram")
      return json(200, {
        configured: true,
        mode,
        receive: mode === "send_receive" ? { state: "receiving" } : undefined,
        owners: { count: 1 },
      });
    if (call.url === "/api/gateway/telegram/owners")
      return json(200, {
        owners: [{ id: "51234890", label: "Zhang San" }],
        pairing: { code: "482913", expires_in: 581 },
        ignored: 0,
      });
    return new Response(null, { status: 404 });
  };
}

test("a configured channel shows its mode, the /start code and the allowed accounts", async () => {
  receiving();
  const page = await harness.render(mount, "zh");
  await settle();
  try {
    expect(harness.calls.map((call) => [call.method, call.url])).toEqual([
      ["GET", "/api/gateway/telegram"],
      ["GET", "/api/gateway/telegram/owners"],
    ]);
    expect(
      [...page.root.querySelectorAll<HTMLElement>(".bc-form > .bc-row")]
        .filter((row) => !row.hidden)
        .map((row) => row.querySelector(".bc-title")?.textContent),
    ).toEqual(["通道", "机器人", "模式", "授权账号", undefined]);
    expect(
      page.query<HTMLInputElement>('input[value="send_receive"]')?.checked,
    ).toBe(true);
    expect(
      page.query("[role=radiogroup] + [role=status] .bc-badge--signal")
        ?.textContent,
    ).toBe("收发中");
    const code = page.query(".bc-form .bc-code-display")!;
    expect(code.textContent).toBe("/start 482913");
    expect(code.previousElementSibling?.textContent).toBe(
      "在 Telegram 里给 Bot 发送",
    );
    expect(page.text()).toContain("有效期还剩 9:41");
    expect(page.text()).toContain("Zhang San51234890移除");

    harness.calls.length = 0;
    receiving("send");
    page.query<HTMLInputElement>('input[value="send"]')!.click();
    await settle();
    expect(
      harness.calls.map((call) => [call.method, call.url, call.body]),
    ).toEqual([
      ["POST", "/api/gateway/telegram/mode", { mode: "send" }],
      ["GET", "/api/gateway/telegram", undefined],
    ]);
    expect(page.refreshes.count).toBe(1);
    expect(
      page.query("[role=radiogroup] + [role=status] .bc-badge")?.textContent,
    ).toBe("仅发送");
  } finally {
    page.unmount();
  }
});

test("the mode and accounts rows in English", async () => {
  receiving();
  const page = await harness.render(mount, "en");
  await settle();
  try {
    for (const phrase of [
      "Mode",
      "Choose how the device uses this channel",
      "Send and receive",
      "Receiving",
      "Allowed accounts",
      "Accounts that can command the device",
      "In Telegram, send the bot",
      "Expires in 9:41",
      "New code",
      "Remove",
    ])
      expect(page.text()).toContain(phrase);
  } finally {
    page.unmount();
  }
});
