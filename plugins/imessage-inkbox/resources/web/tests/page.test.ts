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
    ["GET", "/api/gateway/inkbox"],
  ]);
  harness.calls.length = 0;
  await settle();
  return page;
}

const ENDPOINT = "/api/gateway/inkbox";

type Page = Awaited<ReturnType<typeof harness.render>>;

/** Titles of the rows a person sees. */
const visibleRows = (page: Page) =>
  [...page.root.querySelectorAll<HTMLElement>(".bc-form > .bc-row")]
    .filter((row) => !row.hidden)
    .map(
      (row) =>
        row.querySelector(".bc-title")?.textContent ??
        row.querySelector(".bc-disclosure")?.textContent,
    );
const footer = (page: Page) => page.query(".bc-form__footer")!;
const visible = (node: HTMLElement) => !node.hidden;
const button = (page: Page, label: string) =>
  [...page.root.querySelectorAll<HTMLElement>("button")].find(
    (node) => node.textContent === label,
  )!;

async function signUp(page: Page) {
  harness.reply = async () =>
    json(200, {
      email_address: "barracuda-a1b2c3@inkboxmail.com",
      claim_status: "agent_unclaimed",
    });
  page.type("email", "you@example.com");
  await page.click("发送验证码");
}

test("renders the email method in Chinese: radio, email row, open fold, no footer", async () => {
  const page = await render("zh");
  expect(page.query(".bc-header h1")?.textContent).toBe("Inkbox");
  // the icon is fetched through the shell's queue; the image is there before it arrives
  expect(page.query(".bc-header h1 img")).not.toBeNull();
  expect(page.query(".bc-lead")?.textContent).toBe(
    "接入 Inkbox 身份与邮件服务。",
  );
  expect(visibleRows(page)).toEqual(["方式", "邮箱", "高级"]);
  const cards = [
    ...page.root.querySelectorAll('[aria-label="方式"] .bc-radio-card'),
  ];
  expect(cards.map((card) => card.textContent)).toEqual([
    "用邮箱新建验证码发到你的邮箱",
    "已有 API Key从 Inkbox 控制台复制",
  ]);
  expect(cards.every((card) => card.querySelector(".bc-option-icon svg"))).toBe(
    true,
  );
  expect(page.input("method").checked).toBe(true);
  expect(page.input("email").type).toBe("email");
  expect(page.input("email").placeholder).toBe("you@example.com");
  expect(visible(button(page, "发送验证码"))).toBe(true);
  expect(page.input("api_base").value).toBe("https://inkbox.ai");
  expect(visible(footer(page))).toBe(false);
});

test("renders in English", async () => {
  const page = await render("en");
  for (const phrase of [
    "Connect Inkbox identity and mail.",
    "Create an Inkbox identity, or use one you have",
    "Create with email",
    "I have an API key",
    "Used to claim this Inkbox identity",
    "Send code",
  ])
    expect(page.text()).toContain(phrase);
});

test("email → code → claimed, through the device's signup, resend and verify", async () => {
  const page = await render("zh");
  await page.click("发送验证码");
  expect(harness.calls).toHaveLength(0);
  expect(page.text()).toContain("请填写邮箱地址。");

  await signUp(page);
  expect(
    harness.calls.map((call) => [call.url, call.method, call.body]),
  ).toEqual([
    ["/api/gateway/inkbox/signup", "POST", { email: "you@example.com" }],
    // the signup stored the channel: read it again for its mode and accounts
    ["/api/gateway/inkbox", "GET", undefined],
  ]);
  expect(page.toasts.at(-1)).toEqual({
    kind: "success",
    title: "验证码已发到 you@example.com",
  });
  expect(visibleRows(page)).toEqual(["方式", "邮箱", "验证码", "高级"]);
  expect(visible(button(page, "发送验证码"))).toBe(false);
  expect(page.text()).toContain("验证码已发到you@example.com·重新发送");
  expect(page.input("code").maxLength).toBe(6);
  expect(page.input("code").inputMode).toBe("numeric");
  // the code field is the Input card's code variant, spaced by its class
  expect(page.input("code").classList.contains("bc-input--code")).toBe(true);
  expect(page.input("code").style.letterSpacing).toBe("");

  // resend: accepted, then the cooldown inline next to 重新发送
  harness.reply = async () => new Response(null, { status: 204 });
  await page.click("重新发送");
  expect(harness.calls.at(-1)?.url).toBe("/api/gateway/inkbox/resend");
  expect(page.toasts.at(-1)?.title).toBe("验证码已发到 you@example.com");
  harness.reply = async () =>
    json(422, {
      error: "verification_failed",
      message: "Please wait before resending",
    });
  await page.click("重新发送");
  const resendError = button(page, "重新发送").parentElement!.querySelector(
    ".bc-hint--error",
  )!;
  expect(resendError.textContent).toBe("Please wait before resending");
  expect((resendError as HTMLElement).hidden).toBe(false);

  // verify: the format is checked here, the code by Inkbox
  page.type("code", "12");
  await page.click("验证");
  expect(harness.calls.at(-1)?.url).toBe("/api/gateway/inkbox/resend");
  // a field error is a sentence that says what to do
  expect(
    page.input("code").closest(".bc-field")!.querySelector(".bc-hint--error")
      ?.textContent,
  ).toBe("输入邮件里的 6 位数字。");
  harness.reply = async () =>
    json(422, {
      error: "verification_failed",
      message: "Invalid verification code",
    });
  page.type("code", "000000");
  await page.click("验证");
  expect(harness.calls.at(-1)?.body).toEqual({ code: "000000" });
  expect(
    page.input("code").closest(".bc-field")!.querySelector(".bc-hint--error")!
      .textContent,
  ).toBe("Invalid verification code");

  harness.reply = async () => json(200, { claim_status: "agent_claimed" });
  page.type("code", "123456");
  await page.click("验证");
  expect(harness.calls.at(-1)?.url).toBe("/api/gateway/inkbox/verify");
  expect(visibleRows(page)).toEqual(["方式", "身份", "高级"]);
  const card = page.query(".bc-card[role=status]")!;
  expect(card.querySelector(".bc-option-icon")?.textContent).toBe("@");
  expect(card.querySelector(".bc-option-title")?.textContent).toBe(
    "Inkbox 身份",
  );
  expect(card.textContent).toContain("barracuda-a1b2c3@inkboxmail.com");
  expect(
    card.querySelector(".bc-badge:not(.bc-badge--signal)")?.textContent,
  ).toBe("已认领");
  expect(page.toasts.at(-1)?.title).toBe("Inkbox 身份已认领");
  page.toasts.at(-1)?.action?.run();
  expect(page.routes).toEqual(["imessage-web"]);
});

test("Inkbox's signup refusal is shown on the email field", async () => {
  const page = await render("en");
  harness.reply = async () =>
    json(400, {
      error: "invalid_request",
      message: "value is not a valid email",
    });
  page.type("email", "nope");
  await page.click("Send code");
  expect(
    page.input("email").closest(".bc-field")!.querySelector(".bc-hint--error")!
      .textContent,
  ).toBe("value is not a valid email");
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "Configuration rejected",
    body: "value is not a valid email",
    code: "400",
  });
  expect(visibleRows(page)).toEqual(["Method", "Email", "Advanced"]);
});

test("a verify with no signup on the device goes back to the email step", async () => {
  const page = await render("zh");
  await signUp(page);
  harness.reply = async () => json(409, { error: "conflict" });
  page.type("code", "123456");
  await page.click("验证");
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "配置被拒绝",
    body: undefined,
    code: "409",
  });
  expect(visibleRows(page)).toEqual(["方式", "邮箱", "高级"]);
  expect(visible(button(page, "发送验证码"))).toBe(true);
});

test("Enter in the email field sends the code instead of submitting the form", async () => {
  const page = await render("zh");
  await signUp(page);
  harness.calls.length = 0;
  page.type("email", "other@example.com");
  expect(visible(button(page, "发送验证码"))).toBe(true);
  page.input("email").dispatchEvent(
    new harness.browser.window.KeyboardEvent("keydown", {
      key: "Enter",
      bubbles: true,
      cancelable: true,
    }) as unknown as Event,
  );
  await settle();
  expect(harness.calls.map((call) => call.url)).toEqual([
    "/api/gateway/inkbox/signup",
    "/api/gateway/inkbox",
  ]);
});

test("已有 API Key posts the key, identity and API base with the footer", async () => {
  const page = await render("zh");
  const key = page.root.querySelector<HTMLInputElement>(
    'input[name="method"][value="key"]',
  )!;
  key.click();
  await settle();
  expect(visibleRows(page)).toEqual(["方式", "账号", "高级"]);
  expect(visible(footer(page))).toBe(true);
  expect(page.query(".bc-row__label a")?.getAttribute("href")).toBe(
    "https://inkbox.ai",
  );
  page.type("api_key", "ApiKey_1");
  page.type("identity_id", "6f1c");
  await page.submit();
  expect(harness.calls.at(-1)?.method).toBe("GET");
  const save = harness.calls.at(-2)!;
  expect([save.url, save.method, save.body]).toEqual([
    "/api/gateway/inkbox",
    "POST",
    { api_key: "ApiKey_1", identity_id: "6f1c", api_base: "https://inkbox.ai" },
  ]);
  expect(page.toasts.at(-1)?.title).toBe("设备已接受配置");
  expect(page.input("api_key").value).toBe("");
});

/** Renders with the device's `GET /api/gateway/inkbox` answering `state`. */
async function renderWith(state: unknown, lang: "zh" | "en" = "zh") {
  harness.reply = async () => json(200, state);
  const page = await harness.render(mount, lang);
  await settle();
  harness.calls.length = 0;
  return page;
}

test("a reload resumes a signup at the code step", async () => {
  for (const lang of ["zh", "en"] as const) {
    const page = await renderWith(
      {
        configured: true,
        signup: {
          email_address: "barracuda-a1b2c3@inkboxmail.com",
          claim_status: "agent_unclaimed",
        },
      },
      lang,
    );
    const zh = lang === "zh";
    expect(visibleRows(page)).toEqual(
      zh
        ? ["方式", "邮箱", "验证码", "高级"]
        : ["Method", "Email", "Code", "Advanced"],
    );
    // the address the code went to is not stored on the device
    expect(page.text()).toContain(
      zh ? "验证码已发到你的邮箱·重新发送" : "Code sent to your inbox·Resend",
    );
    expect(visible(button(page, zh ? "发送验证码" : "Send code"))).toBe(false);
    harness.reply = async () => json(200, { claim_status: "agent_claimed" });
    page.type("code", "123456");
    await page.click(zh ? "验证" : "Verify");
    expect(harness.calls.at(-1)?.url).toBe("/api/gateway/inkbox/verify");
    expect(visibleRows(page)).toEqual(
      zh ? ["方式", "身份", "高级"] : ["Method", "Identity", "Advanced"],
    );
    expect(page.query(".bc-card[role=status]")?.textContent).toContain(
      "barracuda-a1b2c3@inkboxmail.com",
    );
    expect(page.refreshes.count).toBe(1);
    page.unmount();
  }
});

test("a reload of a claimed signup shows the identity", async () => {
  const page = await renderWith({
    configured: true,
    signup: {
      email_address: "barracuda-a1b2c3@inkboxmail.com",
      claim_status: "agent_claimed",
    },
  });
  expect(visibleRows(page)).toEqual(["方式", "身份", "高级"]);
  const card = page.query(".bc-card[role=status]")!;
  expect(
    card.querySelector(".bc-badge:not(.bc-badge--signal)")?.textContent,
  ).toBe("已认领");
  expect(card.querySelector(".bc-mono")?.textContent).toBe(
    "barracuda-a1b2c3@inkboxmail.com",
  );
});

test("a channel saved with a key shows as configured above the form", async () => {
  const page = await renderWith({ configured: true }, "en");
  expect(visibleRows(page)).toEqual(["Channel", "Method", "Email", "Advanced"]);
  const card = page.query(".bc-card[role=status]")!;
  expect(card.querySelector(".bc-option-title")?.textContent).toBe("Inkbox");
  expect(card.querySelector("svg.bc-success")).not.toBeNull();
  expect(card.querySelector(".bc-badge")).toBeNull();
  // not configured: nothing to show
  page.unmount();
  const empty = await renderWith({ configured: false });
  expect(visibleRows(empty)).toEqual(["方式", "邮箱", "高级"]);
});

test("a signup the device kept offers 重试, which resumes it with the same email", async () => {
  const page = await render("zh");
  harness.reply = async () =>
    json(422, {
      error: "registration_failed",
      message: "channel rejected",
      retry: true,
    });
  page.type("email", "you@example.com");
  await page.click("发送验证码");
  const toast = page.toasts.at(-1)!;
  expect([toast.title, toast.body, toast.code]).toEqual([
    "配置被拒绝",
    "channel rejected",
    "422",
  ]);
  expect(toast.action?.label).toBe("重试");
  expect(page.refreshes.count).toBe(0);

  harness.reply = async () =>
    json(200, {
      email_address: "barracuda-a1b2c3@inkboxmail.com",
      claim_status: "agent_unclaimed",
    });
  toast.action!.run();
  await settle();
  expect(harness.calls.map((call) => [call.url, call.body])).toEqual([
    ["/api/gateway/inkbox/signup", { email: "you@example.com" }],
    ["/api/gateway/inkbox/signup", { email: "you@example.com" }],
    ["/api/gateway/inkbox", undefined],
  ]);
  expect(visibleRows(page)).toEqual(["方式", "邮箱", "验证码", "高级"]);
  expect(page.refreshes.count).toBe(1);

  // a refusal without the flag offers nothing to retry
  page.unmount();
  harness.calls.length = 0;
  harness.reply = async () => new Response(null, { status: 204 });
  const other = await render("en");
  harness.reply = async () =>
    json(400, { error: "invalid_request", message: "bad email" });
  other.type("email", "nope");
  await other.click("Send code");
  expect(other.toasts.at(-1)?.action).toBeUndefined();
});

test("a key saved through the form shows as configured and refreshes the status", async () => {
  const page = await render("zh");
  page.root
    .querySelector<HTMLInputElement>('input[name="method"][value="key"]')!
    .click();
  await settle();
  page.type("api_key", "ApiKey_1");
  page.type("identity_id", "6f1c");
  harness.reply = async () => json(422, { error: "registration_failed" });
  await page.submit();
  expect(page.refreshes.count).toBe(0);
  page.type("api_key", "ApiKey_1");
  harness.reply = async () => new Response(null, { status: 204 });
  await page.submit();
  expect(page.refreshes.count).toBe(1);
  expect(visibleRows(page)).toEqual(["通道", "方式", "账号", "高级"]);
});

test("a reload with the person's address names it and prefills the email", async () => {
  const page = await renderWith({
    configured: true,
    signup: {
      email_address: "barracuda-a1b2c3@inkboxmail.com",
      claim_status: "agent_unclaimed",
      human_email: "you@example.com",
    },
  });
  expect(visibleRows(page)).toEqual(["方式", "邮箱", "验证码", "高级"]);
  expect(page.text()).toContain("验证码已发到you@example.com·重新发送");
  expect(page.input("email").value).toBe("you@example.com");
  // the address is the one the code went to: no 发送验证码 until it changes
  expect(visible(button(page, "发送验证码"))).toBe(false);
  page.type("email", "other@example.com");
  expect(visible(button(page, "发送验证码"))).toBe(true);
});

test("a claimed identity shows its mode and the code to email from your inbox", async () => {
  harness.reply = async (call) => {
    if (call.url === ENDPOINT)
      return json(200, {
        configured: true,
        mode: "disabled",
        owners: { count: 1 },
        signup: {
          email_address: "barracuda-a1b2c3@inkboxmail.com",
          claim_status: "agent_claimed",
        },
      });
    if (call.url === `${ENDPOINT}/owners`)
      return json(200, {
        owners: [{ id: "you@example.com", label: "you@example.com" }],
        pairing: { code: "482913", expires_in: 600 },
        ignored: 0,
      });
    return new Response(null, { status: 404 });
  };
  const page = await harness.render(mount, "zh");
  await settle();
  try {
    expect(visibleRows(page)).toEqual([
      "方式",
      "身份",
      "模式",
      "授权账号",
      "高级",
    ]);
    expect(
      page.query<HTMLInputElement>('input[value="disabled"]')?.checked,
    ).toBe(true);
    expect(
      page.query("[role=radiogroup] + [role=status] .bc-badge")?.textContent,
    ).toBe("已停用");
    expect(page.text()).toContain("从你的邮箱给这个地址发送482913");
  } finally {
    page.unmount();
  }
  const en = await harness.render(mount, "en");
  await settle();
  try {
    expect(en.text()).toContain("From your inbox, email this address482913");
  } finally {
    en.unmount();
  }
});
