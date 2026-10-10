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
    ["GET", "/api/gateway/bluebubbles/status"],
  ]);
  harness.calls.length = 0;
  await settle();
  return page;
}

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
  const page = await render("zh");
  const { root } = page;
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("BlueBubbles");
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "连接 BlueBubbles 服务器以接入 iMessage。",
  );
  expect(
    [...root.querySelectorAll(".bc-row__label .bc-title")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["通道", "服务器", "选项", "模式", "授权账号"]);
  // 模式 and 授权账号 wait for a configured channel too
  expect(
    [...root.querySelectorAll<HTMLElement>(".bc-form > .bc-row")]
      .filter((row) => !row.hidden)
      .map((row) => row.querySelector(".bc-title")?.textContent),
  ).toEqual(["服务器", "选项", undefined]);
  // the 通道 row waits for the device to say a channel is configured
  expect(root.querySelector<HTMLElement>(".bc-row")?.hidden).toBe(true);
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
  const page = await render("en");
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
  const page = await render("zh");
  harness.reply = async () => json(200, INFO(false));
  page.type("server_url", "https://bb.example.com/");
  page.type("password", "p&w d");
  await page.click("测试连接");
  expect(harness.calls.map((call) => call.url)).toEqual([
    "https://bb.example.com/api/v1/server/info?password=p%26w%20d",
  ]);
  const card = page.query(".bc-card[role=status]")!;
  expect(card.querySelector(".bc-option-title")?.textContent).toBe(
    "BlueBubbles Server",
  );
  expect(card.querySelector(".bc-badge--signal")?.textContent).toBe("已连接");
  expect(card.textContent).toContain("https://bb.example.com");
  expect(card.textContent).toContain("服务器版本1.9.9");
  expect(card.textContent).toContain("macOS15.5");
  expect(card.textContent).toContain("Private API未启用");
  // the server's facts are a key-value table: versions mono, the Private API state in words
  expect(
    [...card.querySelectorAll(".bc-kv dd")].map((node) =>
      node.classList.contains("bc-mono"),
    ),
  ).toEqual([true, true, false]);
  expect(privateApi(page.root).getAttribute("aria-checked")).toBe("false");

  harness.reply = async () => json(200, INFO(true));
  await page.click("测试连接");
  expect(privateApi(page.root).getAttribute("aria-checked")).toBe("true");
  expect(page.text()).toContain("已按服务器设置开启");
  expect(page.query(".bc-card[role=status]")?.textContent).toContain(
    "Private API已启用",
  );

  page.type("password", "other");
  expect(page.query(".bc-card[role=status]")).toBeNull();
  expect(page.text()).toContain("需要服务器已启用 Private API");
});

test("a refused password is shown on the URL field", async () => {
  const page = await render("en");
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
  // the field says what to do; the server's status and words follow in mono
  expect(error.textContent).toBe(
    "Check the server URL and password, then test again. · 401 You are not authorized to access this resource",
  );
  expect(error.querySelector(".bc-mono")?.textContent).toBe(
    "401 You are not authorized to access this resource",
  );
});

test("an unreachable server shows a note, and saving still posts the settings", async () => {
  const page = await render("zh");
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
  // the accepted save reads the channel again for its mode and accounts
  expect(harness.calls.slice(-2).map((call) => call.method)).toEqual([
    "POST",
    "GET",
  ]);
  const save = harness.calls.at(-2)!;
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
  const page = await render("zh");
  harness.reply = async () =>
    json(422, { error: "registration_failed", message: "bad url" });
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

test("a configured channel shows above the form; a save shows it and refreshes the status", async () => {
  harness.reply = async () => json(200, { configured: true });
  const shown = await harness.render(mount, "en");
  await settle();
  const current = shown.query(".bc-form > .bc-row")!;
  expect(current.hidden).toBe(false);
  expect(current.querySelector(".bc-row__label")?.textContent).toBe("Channel");
  expect(current.querySelector(".bc-option-title")?.textContent).toBe(
    "BlueBubbles",
  );
  expect(current.querySelector("svg.bc-success")).not.toBeNull();
  expect(current.querySelector(".bc-badge")).toBeNull();
  shown.unmount();

  harness.reply = async () => json(200, { configured: false });
  const page = await harness.render(mount, "zh");
  await settle();
  const row = page.query(".bc-form > .bc-row")!;
  expect(row.hidden).toBe(true);
  page.type("server_url", "https://bluebubbles.example.com");
  page.type("password", "s3cret");
  harness.reply = async () => json(422, { error: "registration_failed" });
  await page.submit();
  expect(page.refreshes.count).toBe(0);
  expect(row.hidden).toBe(true);
  harness.reply = async () => new Response(null, { status: 204 });
  await page.submit();
  expect(page.refreshes.count).toBe(1);
  expect(row.hidden).toBe(false);
  expect(row.querySelector("svg.bc-success")).not.toBeNull();
  expect(row.querySelector(".bc-badge")).toBeNull();
});

test("a configured channel shows its mode and the code to send from iMessage", async () => {
  harness.reply = async (call) => {
    if (call.url === "/api/gateway/bluebubbles/status")
      return json(200, {
        configured: true,
        mode: "send",
        owners: { count: 2 },
      });
    if (call.url === "/api/gateway/bluebubbles/owners")
      return json(200, {
        owners: [
          { id: "+14155550134", label: "Me" },
          { id: "me@icloud.com", label: null },
        ],
        pairing: { code: "482913", expires_in: 600 },
        ignored: 0,
      });
    return new Response(null, { status: 404 });
  };
  const page = await harness.render(mount, "zh");
  await settle();
  try {
    expect(page.query<HTMLInputElement>('input[value="send"]')?.checked).toBe(
      true,
    );
    expect(
      page.query("[role=radiogroup] + [role=status] .bc-badge")?.textContent,
    ).toBe("仅发送");
    expect(page.text()).toContain("用 iMessage 给这台 Mac 发送482913");
    expect(page.text()).toContain("Me+14155550134移除me@icloud.com移除");
  } finally {
    page.unmount();
  }
  const en = await harness.render(mount, "en");
  await settle();
  try {
    expect(en.text()).toContain("From iMessage, send this Mac482913");
    expect(en.text()).toContain("Send only");
  } finally {
    en.unmount();
  }
});
