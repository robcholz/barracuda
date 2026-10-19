import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  json,
  pageHarness,
  settle,
} from "../../../../captive-portal/resources/web/tests/page";
import { mount } from "../entry";

const waiting = {
  id: "counter",
  name: "Counter",
  version: "0.1.0",
  digest: "ab".repeat(32),
  signer: "3f9a21c0aa55bb66",
  asks: ["在设备上保存数据"],
};
const listing = (overrides: Record<string, unknown> = {}) => ({
  waiting,
  offers: [{ id: "air-quality", name: "Air quality", version: "0.2.0" }],
  installed: [],
  hotspot: false,
  ...overrides,
});

let harness: ReturnType<typeof pageHarness>;
beforeEach(() => {
  harness = pageHarness();
});
afterEach(async () => {
  delete (harness.browser.window as unknown as { opener?: unknown }).opener;
  await harness.close();
});

test("only a browser on the setup hotspot gets the install buttons", async () => {
  harness.reply = async () => json(200, listing());
  const away = await harness.render(mount);
  await settle();
  expect(away.text()).toContain("安装「Counter」？");
  expect(away.text()).toContain("连上设备热点的手机");
  expect(
    [...away.root.querySelectorAll("button")].map((b) => b.textContent),
  ).not.toContain("安装");
  away.unmount();

  harness.reply = async (call) =>
    call.url === "/api/apps"
      ? json(200, listing({ hotspot: true }))
      : json(200, waiting);
  const here = await harness.render(mount);
  await settle();
  await here.click("安装");
  const install = harness.calls.find(
    (call) => call.url === "/api/apps/install",
  );
  expect(install?.body).toEqual({ id: "counter", digest: waiting.digest });
});

/** Opens the page as Live Editor does: from another window, which hears what happens. */
async function openedByLiveEditor() {
  const heard: { message: Record<string, unknown>; origin: string }[] = [];
  const opener = {
    postMessage: (message: Record<string, unknown>, origin: string) =>
      heard.push({ message, origin }),
  };
  Object.defineProperty(harness.browser.window, "opener", {
    value: opener,
    configurable: true,
  });
  const page = await harness.render(mount);
  await settle();
  // happy-dom has no MessageEvent: a plain event carries what one would.
  const deliver = (data: unknown, source: unknown = opener) => {
    const event = new harness.browser.window.Event("message");
    Object.defineProperties(event, {
      data: { value: data },
      origin: { value: "https://live.barracuda.dev" },
      source: { value: source },
    });
    harness.browser.window.dispatchEvent(event);
  };
  return { page, heard, opener, deliver };
}

const offer = (bytes = new Uint8Array([66, 65, 80, 80]).buffer) => ({
  type: "barracuda/offer",
  v: 1,
  id: "counter",
  name: "Counter",
  version: "0.1.0",
  bapp: bytes,
});

test("Live Editor's package reaches the device only when the person sends it", async () => {
  let installed = false;
  harness.reply = async (call) => {
    if (call.url === "/api/apps/offer/counter") return json(201, waiting);
    return json(
      200,
      installed
        ? listing({
            waiting: null,
            installed: [{ id: "counter", version: "0.1.0" }],
          })
        : listing({ waiting: null }),
    );
  };
  const { page, heard, deliver } = await openedByLiveEditor();
  expect(heard[0]).toEqual({
    message: { type: "barracuda/ready", v: 1 },
    origin: "*",
  });

  deliver(offer());
  await settle();
  expect(page.text()).toContain("Live Editor 发来一个应用");
  expect(page.text()).toContain("https://live.barracuda.dev");
  expect(harness.calls.some((call) => call.method === "PUT")).toBe(false);

  installed = true;
  await page.click("发送到设备");
  const put = harness.calls.find((call) => call.method === "PUT");
  expect(put?.url).toBe("/api/apps/offer/counter");
  expect(put?.body).toBeInstanceOf(ArrayBuffer);
  const states = heard
    .filter(({ message }) => message.type === "barracuda/status")
    .map(({ message, origin }) => [message.state, origin]);
  expect(states).toEqual([
    ["review", "https://live.barracuda.dev"],
    ["sending", "https://live.barracuda.dev"],
    ["waiting", "https://live.barracuda.dev"],
    ["installed", "https://live.barracuda.dev"],
  ]);
  expect(page.text()).toContain("已安装");
});

test("a declined or refused package is reported, and strangers are ignored", async () => {
  harness.reply = async (call) =>
    call.url === "/api/apps/offer/counter"
      ? json(201, waiting)
      : json(200, listing({ waiting: null }));
  const { page, heard, deliver } = await openedByLiveEditor();

  deliver(offer(), {});
  deliver({ ...offer(), id: "../escape" });
  deliver({ ...offer(), bapp: new ArrayBuffer(0) });
  await settle();
  expect(page.text()).not.toContain("Live Editor 发来一个应用");

  deliver(offer());
  await settle();
  await page.click("不发送");
  expect(heard.at(-1)?.message.state).toBe("refused");
  expect(harness.calls.some((call) => call.method === "PUT")).toBe(false);

  deliver(offer());
  await settle();
  await page.click("发送到设备");
  expect(heard.at(-1)?.message.state).toBe("declined");
});

test("a refused upload goes back to the review, with the device's reason", async () => {
  harness.reply = async (call) =>
    call.url === "/api/apps/offer/counter"
      ? json(409, { error: "incompatible", message: "需要摄像头" })
      : json(200, listing({ waiting: null }));
  const { page, heard, deliver } = await openedByLiveEditor();
  deliver(offer());
  await settle();
  await page.click("发送到设备");
  expect(heard.at(-1)?.message).toMatchObject({
    state: "failed",
    message: "需要摄像头",
  });
  expect(page.toasts.length).toBe(1);
  expect(
    [...page.root.querySelectorAll("button")].map((b) => b.textContent),
  ).toContain("发送到设备");
});
