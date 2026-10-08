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

/** BlueBubbles Server's reply shape (`Success` with `GeneralInterface.getServerMetadata()`). */
const INFO = (privateApi: boolean) => ({
  status: 200,
  message: "Success",
  data: {
    computer_id: "mac",
    os_version: "15.5",
    server_version: "1.9.9",
    private_api: privateApi,
    proxy_service: "Cloudflare",
    helper_connected: true,
  },
});

const privateApi = (root: ParentNode) =>
  root.querySelector<HTMLElement>('[name="use_private_api"]')!;

test("renders the design in Chinese: server row with 测试连接, Private API switch, open fold", async () => {
  const page = await harness.render(mount, "zh");
  const { root } = page;
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("BlueBubbles");
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "连接 BlueBubbles 服务器以接入 iMessage。",
  );
  expect(
    [...root.querySelectorAll(".bc-row__label .bc-title")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["服务器", "选项"]);
  expect(page.text()).toContain(
    "BlueBubbles Server 的「设置 → API」里有地址和密码",
  );
  expect(page.input("server_url").placeholder).toBe(
    "https://bluebubbles.example.com",
  );
  expect(
    [...page.input("password").parentElement!.querySelectorAll("button")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["显示", "测试连接"]);
  expect(privateApi(root).getAttribute("aria-checked")).toBe("true");
  expect(page.text()).toContain("需要服务器已启用 Private API");
  expect(page.input("stream_edit_min_delta_bytes").value).toBe("128");
  expect(page.input("stream_max_edits").value).toBe("4");
});

test("renders in English", async () => {
  const page = await harness.render(mount, "en");
  for (const phrase of [
    "Connect a BlueBubbles server to reach iMessage.",
    "Find both in BlueBubbles Server under Settings → API",
    "Test connection",
    "Use the Private API",
    "Streaming edit minimum delta",
    "Max streaming edits",
  ])
    expect(page.text()).toContain(phrase);
});

test("测试连接 reads the server info and sets the Private API switch to match", async () => {
  const page = await harness.render(mount, "zh");
  harness.reply = async () => json(200, INFO(false));
  page.type("server_url", "https://bb.example.com/");
  page.type("password", "p&w d");
  await page.click("测试连接");
  expect(harness.calls.map((call) => call.url)).toEqual([
    "https://bb.example.com/api/v1/server/info?password=p%26w%20d",
  ]);
  const card = page.query(".bc-frame[role=status]")!;
  expect(card.querySelector(".bc-option-title")?.textContent).toBe(
    "BlueBubbles Server",
  );
  expect(card.querySelector(".bc-badge--signal")?.textContent).toBe("已连接");
  expect(card.textContent).toContain("https://bb.example.com");
  expect(card.textContent).toContain("服务器版本1.9.9");
  expect(card.textContent).toContain("macOS15.5");
  expect(card.textContent).toContain("Private API未启用");
  expect(privateApi(page.root).getAttribute("aria-checked")).toBe("false");

  harness.reply = async () => json(200, INFO(true));
  await page.click("测试连接");
  expect(privateApi(page.root).getAttribute("aria-checked")).toBe("true");
  expect(page.text()).toContain("已按服务器设置开启");
  expect(page.query(".bc-frame[role=status]")?.textContent).toContain(
    "Private API已启用",
  );

  page.type("password", "other");
  expect(page.query(".bc-frame[role=status]")).toBeNull();
  expect(page.text()).toContain("需要服务器已启用 Private API");
});

test("a refused password is shown on the URL field", async () => {
  const page = await harness.render(mount, "en");
  harness.reply = async () =>
    json(401, {
      status: 401,
      message: "You are not authorized to access this resource",
      error: { type: "Authentication Error", message: "Unauthorized" },
    });
  page.type("server_url", "https://bb.example.com");
  page.type("password", "wrong");
  await page.click("Test connection");
  const error = page
    .input("server_url")
    .closest(".bc-field")!
    .querySelector(".bc-hint--error")!;
  expect(error.textContent).toBe(
    "BlueBubbles Server: You are not authorized to access this resource · 401",
  );
});

test("an unreachable server shows a note, and saving still posts the settings", async () => {
  const page = await harness.render(mount, "zh");
  harness.reply = async (call) => {
    if (call.url.startsWith("https://bb.example.com"))
      throw new TypeError("Failed to fetch");
    return new Response(null, { status: 204 });
  };
  page.type("server_url", "https://bb.example.com");
  page.type("password", "pw");
  await page.click("测试连接");
  expect(page.text()).toContain("连不上这台服务器，连接未测试");
  await page.submit();
  const save = harness.calls.at(-1)!;
  expect([save.url, save.method]).toEqual(["/api/gateway/bluebubbles", "POST"]);
  expect(save.body).toEqual({
    server_url: "https://bb.example.com",
    password: "pw",
    use_private_api: true,
    stream_edit_min_delta_bytes: 128,
    stream_max_edits: 4,
  });
  expect(page.toasts.at(-1)?.title).toBe("设备已接受配置");
  expect(page.input("password").value).toBe("");
});

test("a rejected save toasts the device's message", async () => {
  const page = await harness.render(mount, "zh");
  harness.reply = async () =>
    json(422, { error: "invalid_configuration", message: "bad url" });
  page.type("server_url", "https://bb.example.com");
  page.type("password", "pw");
  await page.submit();
  expect(page.toasts.at(-1)).toEqual({
    kind: "error",
    title: "配置被拒绝",
    body: "bad url",
    code: "422",
  });
});
