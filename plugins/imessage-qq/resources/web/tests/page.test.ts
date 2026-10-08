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

const texts = (root: ParentNode, selector: string) =>
  [...root.querySelectorAll(selector)].map((node) => node.textContent);

test("renders the design in Chinese: App ID and App Secret, token URL under 高级, 验证并保存", async () => {
  const page = await harness.render(mount, "zh");
  const { root } = page;
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("QQ");
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "通过 QQ Bot 收发消息。",
  );
  expect(root.querySelector("hl-figure")?.getAttribute("name")).toBe("riffle");
  expect(root.querySelector(".bc-row__label .bc-title")?.textContent).toBe(
    "机器人",
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
  const page = await harness.render(mount, "en");
  for (const phrase of [
    "Send and receive messages through a QQ bot.",
    "Create a bot on the QQ Open Platform and copy these from Development settings",
    "Open the QQ Open Platform",
    "Verify and save",
  ])
    expect(page.text()).toContain(phrase);
});

test("validates, then posts App ID and App Secret and clears the secret", async () => {
  const page = await harness.render(mount, "zh");
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
  const page = await harness.render(mount, "zh");
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
  const page = await harness.render(mount, "en");
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
