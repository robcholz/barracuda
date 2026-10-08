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
    ["GET", "/api/gateway/qq"],
  ]);
  harness.calls.length = 0;
  await settle();
  return page;
}

const texts = (root: ParentNode, selector: string) =>
  [...root.querySelectorAll(selector)].map((node) => node.textContent);

test("renders the design in Chinese: App ID and App Secret, token URL under 高级, 验证并保存", async () => {
  const page = await render("zh");
  const { root } = page;
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("QQ");
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "通过 QQ Bot 收发消息。",
  );
  expect(root.querySelector("hl-figure")?.getAttribute("name")).toBe("riffle");
  expect(
    [...root.querySelectorAll(".bc-row__label .bc-title")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["通道", "机器人"]);
  // the 通道 row waits for the device to say a channel is configured
  expect(root.querySelector<HTMLElement>(".bc-row")?.style.display).toBe(
    "none",
  );
  expect(page.text()).toContain("在 QQ 开放平台创建机器人，从「开发设置」复制");
  expect(root.querySelector(".bc-row__label a")?.getAttribute("href")).toBe(
    "https://q.qq.com",
  );
  expect(page.input("app_id").type).toBe("text");
  expect(page.input("app_secret").type).toBe("password");
  expect(root.querySelector('[name="access_token"]')).toBeNull();
  expect(page.input("api_base").value).toBe("https://api.sgroup.qq.com");
  expect(page.input("token_url").value).toBe(
    "https://bots.qq.com/app/getAppAccessToken",
  );
  expect(texts(root, ".bc-form__footer button")).toEqual([
    "清空",
    "验证并保存",
  ]);
});

test("renders in English", async () => {
  const page = await render("en");
  for (const phrase of [
    "Send and receive messages through a QQ bot.",
    "Create a bot on the QQ Open Platform and copy these from Development settings",
    "Open the QQ Open Platform",
    "Verify and save",
  ])
    expect(page.text()).toContain(phrase);
});

test("validates, then posts App ID and App Secret and clears the secret", async () => {
  const page = await render("zh");
  await page.submit();
  expect(harness.calls).toHaveLength(0);
  expect(page.text()).toContain("请填写 App ID。");
  expect(page.text()).toContain("请填写 App Secret。");

  page.type("app_id", "102345678");
  page.type("app_secret", "s3cret");
  await page.submit();
  expect(harness.calls).toHaveLength(1);
  const [call] = harness.calls;
  expect([call.url, call.method]).toEqual(["/api/gateway/qq", "POST"]);
  expect(call.body).toEqual({
    app_id: "102345678",
    app_secret: "s3cret",
    api_base: "https://api.sgroup.qq.com",
    token_url: "https://bots.qq.com/app/getAppAccessToken",
  });
  expect(page.toasts.at(-1)?.title).toBe("设备已接受配置");
  expect(page.input("app_secret").value).toBe("");
});

test("QQ's rejection lands on the secret field with its code", async () => {
  const page = await render("zh");
  harness.reply = async () =>
    json(422, {
      error: "verification_failed",
      message: "机器人不存在",
      code: "10004",
    });
  page.type("app_id", "102345678");
  page.type("app_secret", "wrong");
  await page.submit();
  const error = page
    .input("app_secret")
    .closest(".bc-field")!
    .querySelector(".bc-hint--error")!;
  expect(error.textContent).toBe("QQ 开放平台：机器人不存在 · 10004");
  expect(error.querySelector(".bc-mono")?.textContent).toBe("10004");
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "配置被拒绝",
    body: "机器人不存在",
    code: "422",
  });
  expect(page.input("app_secret").value).toBe("wrong");
  page.type("app_secret", "right");
  expect(error.textContent).toBe("");
});

test("an unreachable token service toasts the device's message, in English", async () => {
  const page = await render("en");
  harness.reply = async () =>
    json(502, { error: "upstream_unavailable", message: "dns failure" });
  page.type("app_id", "1");
  page.type("app_secret", "s");
  await page.submit();
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "Submission failed",
    body: "dns failure",
    code: "502",
  });
  expect(
    page
      .input("app_secret")
      .closest(".bc-field")!
      .querySelector(".bc-hint--error")!.textContent,
  ).toBe("");
});

test("a configured channel shows above the form; a save shows it and refreshes the status", async () => {
  harness.reply = async () => json(200, { configured: true });
  const shown = await harness.render(mount, "en");
  await settle();
  const current = shown.query(".bc-form > .bc-row")!;
  expect(current.style.display).toBe("");
  expect(current.querySelector(".bc-row__label")?.textContent).toBe("Channel");
  expect(current.querySelector(".bc-option-title")?.textContent).toBe("QQ");
  expect(current.querySelector(".bc-badge--signal")?.textContent).toBe(
    "Configured",
  );
  shown.unmount();

  harness.reply = async () => json(200, { configured: false });
  const page = await harness.render(mount, "zh");
  await settle();
  const row = page.query(".bc-form > .bc-row")!;
  expect(row.style.display).toBe("none");
  page.type("app_id", "102345678");
  page.type("app_secret", "s3cret");
  harness.reply = async () => json(422, { error: "registration_failed" });
  await page.submit();
  expect(page.refreshes.count).toBe(0);
  expect(row.style.display).toBe("none");
  harness.reply = async () => new Response(null, { status: 204 });
  await page.submit();
  expect(page.refreshes.count).toBe(1);
  expect(row.style.display).toBe("");
  expect(row.querySelector(".bc-badge--signal")?.textContent).toBe("已配置");
});
