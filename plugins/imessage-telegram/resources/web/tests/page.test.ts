import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  json,
  pageHarness,
} from "../../../../captive-portal/resources/web/tests/page";
import { mount } from "../entry";

let harness: ReturnType<typeof pageHarness>;
beforeEach(() => {
  harness = pageHarness();
});
afterEach(async () => {
  await harness.close();
});

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
  const page = await harness.render(mount, "zh");
  const { root } = page;
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("Telegram");
  expect(root.querySelector(".bc-header h1 .bc-asset-icon")).not.toBeNull();
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "通过 Telegram Bot 收发消息。",
  );
  expect(root.querySelector("hl-figure")?.getAttribute("name")).toBe("riffle");
  expect(
    [...root.querySelectorAll<HTMLElement>(".bc-form > .bc-row")]
      .filter((row) => row.style.display !== "none")
      .map((row) => row.querySelector(".bc-title")?.textContent),
  ).toEqual(["Bot", undefined]);
  expect(root.querySelector(".bc-row__label .bc-muted")?.textContent).toBe(
    "在 @BotFather 发送 /newbot 获得 Token",
  );
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
  expect(root.querySelector(".bc-form__endpoint")?.textContent).toContain(
    "POST /api/gateway/telegram",
  );
  expect(texts(root, ".bc-form__footer button")).toEqual([
    "清空",
    "保存并替换通道",
  ]);
  expect(harness.calls).toHaveLength(0);
});

test("renders in English", async () => {
  const page = await harness.render(mount, "en");
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
  const page = await harness.render(mount, "zh");
  await page.click("验证");
  expect(harness.calls).toHaveLength(0);
  expect(page.text()).toContain("请填写 Bot Token。");

  harness.reply = async () => json(200, GET_ME);
  page.type("token", " 123:abc ");
  await page.click("验证");
  expect(harness.calls.map((call) => call.url)).toEqual([
    "https://api.telegram.org/bot123:abc/getMe",
  ]);
  const card = page.query(".bc-frame[role=status]")!;
  expect(card.querySelector(".bc-option-icon")?.textContent).toBe("B");
  expect(card.querySelector(".bc-option-title")?.textContent).toBe(
    "Barracuda Home",
  );
  expect(card.textContent).toContain("@barracuda_home_bot");
  expect(card.querySelector(".bc-badge--signal")?.textContent).toBe("已验证");
  const chat = [...page.root.querySelectorAll<HTMLElement>(".bc-row")].find(
    (row) => row.textContent?.startsWith("开始对话"),
  )!;
  expect(chat.style.display).toBe("");
  expect(chat.textContent).toContain("用手机扫码，打开和 Bot 的对话");
  const open = chat.querySelector("a")!;
  expect([open.textContent, open.getAttribute("href")]).toEqual([
    "在 Telegram 中打开",
    "https://t.me/barracuda_home_bot",
  ]);
  expect(chat.querySelector("[data-theme=light] svg path")).not.toBeNull();

  // editing the token drops the stale result
  page.type("token", "123:abd");
  expect(page.query(".bc-frame[role=status]")).toBeNull();
  expect(chat.style.display).toBe("none");
});

test("Telegram's refusal is shown on the token field; the 高级 API base is used", async () => {
  const page = await harness.render(mount, "zh");
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
  expect(error.textContent).toBe(
    "Telegram 拒绝了这个 Token · 401 Unauthorized",
  );
  expect(page.input("token").getAttribute("aria-invalid")).toBe("true");
});

test("no route to Telegram shows a note and saving still works", async () => {
  const page = await harness.render(mount, "en");
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
  const save = harness.calls.at(-1)!;
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
  const page = await harness.render(mount, "zh");
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
