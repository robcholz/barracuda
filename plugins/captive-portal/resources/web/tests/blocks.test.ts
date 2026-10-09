import { afterEach, beforeEach, expect, test } from "bun:test";
import { json, pageHarness } from "./page";
import { QR_VECTORS } from "./qr-vectors";
import {
  button,
  callDevice,
  configuredRow,
  deviceError,
  encodeQr,
  note,
  qrLink,
  qrPath,
  qrPlate,
  qrSvg,
  readChannel,
  resultCard,
  settingsForm,
  stepList,
  toastDeviceError,
  type PortalContext,
  type Toast,
} from "../ui";

let harness: ReturnType<typeof pageHarness>;
beforeEach(() => {
  harness = pageHarness();
});
afterEach(async () => {
  await harness.close();
});

function context(lang: "zh" | "en" = "zh") {
  const controller = new AbortController();
  const toasts: Toast[] = [];
  const value: PortalContext = {
    signal: controller.signal,
    lang,
    toast: (toast) => {
      if (!controller.signal.aborted) toasts.push(toast);
    },
    navigate: () => {},
    status: () => null,
    refreshStatus: async () => {},
  };
  return { context: value, controller, toasts };
}

const rowsOf = (code: ReturnType<typeof encodeQr>) =>
  Array.from({ length: code.size }, (_, y) =>
    Array.from(code.modules.subarray(y * code.size, (y + 1) * code.size)).join(
      "",
    ),
  );

test("the QR encoder matches the reference symbols, versions 1 to 10 and forced masks", () => {
  for (const vector of QR_VECTORS) {
    const code = encodeQr(vector.text, vector.forced ? vector.mask : undefined);
    expect(code.version).toBe(vector.version);
    expect(code.mask).toBe(vector.mask);
    expect(code.size).toBe(vector.version * 4 + 17);
    const expected = vector.rows.map((row) =>
      BigInt(`0x${row}`).toString(2).padStart(code.size, "0"),
    );
    expect(rowsOf(code)).toEqual(expected);
  }
  expect(QR_VECTORS.some((vector) => vector.version >= 10)).toBe(true);
});

test("the QR encoder picks the smallest version and refuses data past version 40", () => {
  expect(encodeQr("").version).toBe(1);
  expect(encodeQr("x".repeat(14)).version).toBe(1);
  expect(encodeQr("x".repeat(15)).version).toBe(2);
  expect(encodeQr("x".repeat(2331)).version).toBe(40);
  expect(() => encodeQr("x".repeat(2332))).toThrow(RangeError);
});

test("qrPath draws one rectangle per dark run inside the quiet zone", () => {
  const code = encodeQr("A");
  const path = qrPath(code, 2);
  // the top-left finder's first row: seven dark modules from (2, 2)
  expect(path.startsWith("M2 2h7v1h-7z")).toBe(true);
  const dark = [...code.modules].filter(Boolean).length;
  const drawn = [...path.matchAll(/h(\d+)v1/g)].reduce(
    (sum, match) => sum + Number(match[1]),
    0,
  );
  expect(drawn).toBe(dark);
  const svg = qrSvg("A", 120);
  expect(svg.getAttribute("viewBox")).toBe("0 0 25 25");
  expect(svg.getAttribute("width")).toBe("120");
  expect(svg.querySelector("path")?.getAttribute("d")).toBe(path);
});

test("qrPlate stays light in either theme and dims the code under a label", () => {
  const plate = qrPlate("https://t.me/x", 168, "en", {
    dim: { zh: "已扫码", en: "Scanned" },
  });
  expect(plate.getAttribute("data-theme")).toBe("light");
  // the QRCode card's markup: the plate, the code as its first child, the overlay over it
  expect(plate.className).toBe("bc-qr bc-qr--dim");
  expect(plate.getAttribute("style")).toBeNull();
  const code = plate.firstElementChild!;
  expect(code.tagName.toLowerCase()).toBe("svg");
  expect(code.getAttribute("width")).toBe("168");
  // the plate fills the modules and dims them; the code carries no look of its own
  expect(code.getAttribute("style")).toBeNull();
  expect(code.querySelector("path")?.getAttribute("style")).toBeNull();
  expect(plate.lastElementChild?.className).toBe("bc-qr__overlay");
  expect(plate.textContent).toBe("Scanned");
  const live = qrPlate("https://t.me/x", 168, "zh");
  expect(live.className).toBe("bc-qr");
  expect(live.querySelector(".bc-qr__overlay")).toBeNull();
  // while a code loads, an empty code-sized space keeps the plate's size
  const empty = qrPlate(null, 168, "zh");
  const space = empty.firstElementChild!;
  expect(space.tagName.toLowerCase()).toBe("svg");
  expect(space.getAttribute("width")).toBe("168");
  expect(space.getAttribute("height")).toBe("168");
  expect(space.childElementCount).toBe(0);
});

test("qrLink shows the URL in mono and opens it in a new tab", () => {
  const card = qrLink(
    "https://t.me/bot",
    { zh: "在 Telegram 中打开", en: "Open in Telegram" },
    "zh",
  );
  expect(card.querySelector(".bc-mono")?.textContent).toBe("https://t.me/bot");
  const open = card.querySelector("a")!;
  expect(open.getAttribute("href")).toBe("https://t.me/bot");
  expect(open.getAttribute("target")).toBe("_blank");
  expect(open.getAttribute("rel")).toBe("noreferrer");
  expect(open.textContent).toBe("在 Telegram 中打开");
  // a Card whose one body holds the same plate as WeChat's
  expect(card.className).toBe("bc-card");
  expect(card.childElementCount).toBe(1);
  expect(card.firstElementChild?.className).toBe("bc-card__body");
  expect(card.querySelector(".bc-card__body > .bc-qr > svg")).not.toBeNull();
  expect(card.getAttribute("style")).toBeNull();
  expect(card.querySelector<HTMLElement>(".bc-card__body")?.style.padding).toBe(
    "",
  );
});

test("resultCard: initial or check tile, mono sub, neutral or live badge, rows and one action", () => {
  const card = resultCard(
    {
      title: "Barracuda Home",
      badge: { zh: "已验证", en: "Verified" },
      sub: "@barracuda_home_bot",
      initial: "B",
      rows: [
        [{ zh: "服务器版本", en: "Server" }, "1.9.9"],
        ["Private API", "Enabled", false],
      ],
      action: button("重新绑定", "zh", { variant: "outline", size: "sm" }),
    },
    "en",
  );
  expect(card.getAttribute("role")).toBe("status");
  // a Card with one body; the card itself carries no inline look
  expect(card.className).toBe("bc-card");
  expect(card.getAttribute("style")).toBeNull();
  expect(card.childElementCount).toBe(1);
  expect(card.firstElementChild?.className).toBe("bc-card__body");
  expect((card.firstElementChild as HTMLElement).style.padding).toBe("");
  expect(card.querySelector(".bc-option-icon")?.textContent).toBe("B");
  expect(card.querySelector(".bc-option-title")?.textContent).toBe(
    "Barracuda Home",
  );
  expect(card.querySelector(".bc-mono.bc-muted")?.textContent).toBe(
    "@barracuda_home_bot",
  );
  // a verified result is a neutral badge; the tile sets the initial at 500 itself
  expect(card.querySelector(".bc-badge--signal")).toBeNull();
  expect(card.querySelector(".bc-badge")?.textContent).toBe("Verified");
  expect(
    card.querySelector(".bc-option-icon")?.getAttribute("style"),
  ).toBeNull();
  // the rows are a key-value table; machine values mono, words not
  expect(
    [...card.querySelectorAll(".bc-kv dd")].map((node) => [
      node.textContent,
      node.classList.contains("bc-mono"),
    ]),
  ).toEqual([
    ["1.9.9", true],
    ["Enabled", false],
  ]);
  expect(card.textContent).toContain("Server1.9.9");
  // only a live connection is the signal chip
  const live = resultCard(
    { title: "BlueBubbles Server", badge: "已连接", live: true },
    "zh",
  );
  expect(live.querySelector(".bc-badge--signal")?.textContent).toBe("已连接");
  expect(card.querySelector("button")?.textContent).toBe("重新绑定");
  const bare = resultCard({ title: "微信已绑定", badge: "已绑定" }, "zh");
  expect(bare.querySelector(".bc-option-icon svg")).not.toBeNull();
});

test("stepList marks done, current and later steps", () => {
  const list = stepList(["一", "二", "三"], { current: 1, done: 1 }, "zh");
  const items = [...list.querySelectorAll("li")];
  expect(items.map((item) => item.getAttribute("aria-current"))).toEqual([
    null,
    "step",
    null,
  ]);
  expect(items[0].querySelector("svg")).not.toBeNull();
  expect(items[1].textContent).toBe("02二");
  expect(items[2].textContent).toBe("03三");
  // the Steps card: done is .bc-step--done (a success check), current takes aria-current (a
  // muted fill), the marks and labels carry no inline look
  expect(list.className).toBe("bc-steps");
  expect(items.map((item) => item.className)).toEqual([
    "bc-step bc-step--done",
    "bc-step",
    "bc-step",
  ]);
  for (const item of items) {
    const mark = item.firstElementChild as HTMLElement;
    expect(mark.className).toBe("bc-step__mark");
    expect(mark.getAttribute("aria-hidden")).toBe("true");
    for (const node of [item, mark, item.lastElementChild!])
      expect(node.getAttribute("style")).toBeNull();
  }
  const none = stepList(["一", "二"], { current: -1, done: 0 }, "zh");
  expect(none.querySelector("[aria-current]")).toBeNull();
});

test("note: text, mono value and a link-styled action", () => {
  let clicked = 0;
  const line = note({ zh: "验证码已发到", en: "Code sent to" }, "en", {
    mono: "you@example.com",
    action: { label: "Resend", onClick: () => clicked++ },
  });
  expect(line.textContent).toBe("Code sent toyou@example.com·Resend");
  // the value is mono and stays muted with the line; the action is a bare button.bc-link
  expect(line.querySelector(".bc-mono")?.getAttribute("style")).toBeNull();
  expect(line.querySelector("button")?.className).toBe("bc-link");
  expect(line.querySelector("button")?.getAttribute("style")).toBeNull();
  line.querySelector("button")!.click();
  expect(clicked).toBe(1);
});

test("deviceError and callDevice read the contract's error body", async () => {
  expect(
    await deviceError(
      json(422, {
        error: "verification_failed",
        message: "机器人不存在",
        code: 10004,
      }),
    ),
  ).toEqual({
    status: 422,
    error: "verification_failed",
    message: "机器人不存在",
    code: "10004",
  });
  expect(await deviceError(new Response("<html>", { status: 502 }))).toEqual({
    status: 502,
    error: "",
  });

  const { context: ctx, toasts, controller } = context();
  harness.reply = async () => json(200, { url: "https://x", expires_in: 480 });
  expect(await callDevice(ctx, "/api/x", { method: "POST", body: {} })).toEqual(
    {
      kind: "ok",
      status: 200,
      data: { url: "https://x", expires_in: 480 },
    },
  );
  const call = harness.calls[0];
  expect([call.url, call.method, call.body]).toEqual(["/api/x", "POST", {}]);
  expect(call.init.redirect).toBe("error");
  expect(call.init.cache).toBe("no-store");

  harness.reply = async () => json(409, { error: "conflict", message: "busy" });
  const failed = await callDevice(ctx, "/api/x");
  expect(failed).toEqual({
    kind: "error",
    error: { status: 409, error: "conflict", message: "busy" },
  });
  if (failed.kind === "error") toastDeviceError(ctx, failed);
  expect(toasts.at(-1)).toEqual({
    kind: "error",
    title: "配置被拒绝",
    body: "busy",
    code: "409",
  });

  harness.reply = async () => {
    throw new TypeError("offline");
  };
  const offline = await callDevice(ctx, "/api/x");
  expect(offline).toEqual({ kind: "offline" });
  if (offline.kind === "offline") toastDeviceError(ctx, offline, () => {});
  expect(toasts.at(-1)?.title).toBe("未收到设备确认");
  expect(toasts.at(-1)?.action?.label).toBe("重试");

  controller.abort();
  expect(await callDevice(ctx, "/api/x")).toEqual({ kind: "aborted" });
});

test("settingsForm extensions: field action, row link and blocks, coded errors, footer, fold toggle, onError", async () => {
  const { context: ctx, toasts } = context();
  const verify = button("验证", "zh", { variant: "outline" });
  const block = document.createElement("div");
  block.className = "result";
  const toggles: boolean[] = [];
  const form = settingsForm(
    {
      endpoint: "/api/gateway/qq",
      submit: "验证并保存",
      rows: [
        {
          title: "Bot",
          link: { label: "打开 @BotFather", href: "https://t.me/BotFather" },
          fields: [
            {
              kind: "secret",
              name: "token",
              label: "Bot Token",
              action: verify,
            },
            {
              kind: "text",
              name: "app_id",
              label: "App ID",
              action: button("x", "zh"),
            },
          ],
          blocks: block,
        },
      ],
      advanced: {
        onToggle: (open) => toggles.push(open),
        fields: [
          {
            kind: "url",
            name: "api_base",
            label: "API Base URL",
            value: "bad",
          },
        ],
      },
      onError: (error) => {
        form.setError("token", `QQ 开放平台：${error.message}`, error.code);
        return { body: error.message };
      },
    },
    ctx,
  );
  const token = form.element.querySelector<HTMLInputElement>('[name="token"]')!;
  const line = token.parentElement!;
  expect([...line.children].map((node) => node.textContent)).toEqual([
    "",
    "显示",
    "验证",
  ]);
  const appId =
    form.element.querySelector<HTMLInputElement>('[name="app_id"]')!;
  expect(appId.parentElement!.lastElementChild?.textContent).toBe("x");
  expect(appId.closest("label")).toBeNull();
  const link =
    form.element.querySelector<HTMLAnchorElement>(".bc-row__label a")!;
  expect(link.textContent).toBe("打开 @BotFather");
  expect(link.getAttribute("target")).toBe("_blank");
  // the external-link icon is a plain 16px icon
  expect(link.querySelector("svg")?.getAttribute("style")).toBeNull();
  expect(form.element.querySelector(".bc-row__body > .result")).toBe(block);
  expect(form.footer.classList.contains("bc-form__footer")).toBe(true);

  const foldBody =
    form.element.querySelector<HTMLInputElement>('[name="api_base"]')!
      .parentElement!.parentElement!;
  expect(foldBody.hidden).toBe(true);
  form.element.querySelector<HTMLButtonElement>(".bc-disclosure")!.click();
  expect(foldBody.style.display).toBe("flex");
  form.element.querySelector<HTMLButtonElement>(".bc-disclosure")!.click();
  expect(foldBody.hidden).toBe(true);
  expect(toggles).toEqual([true, false]);
  // an invalid field in the closed fold opens it, and says so
  token.value = "s";
  appId.value = "1";
  expect(form.values()).toBeNull();
  expect(toggles).toEqual([true, false, true]);

  form.element.querySelector<HTMLInputElement>('[name="api_base"]')!.value =
    "https://api.sgroup.qq.com";
  harness.reply = async () =>
    json(422, {
      error: "verification_failed",
      message: "机器人不存在",
      code: "10004",
    });
  expect(await form.submit()).toBe("rejected");
  const error = token.closest(".bc-field")!.querySelector(".bc-hint--error")!;
  expect(error.textContent).toBe("QQ 开放平台：机器人不存在 · 10004");
  expect(error.querySelector(".bc-mono")?.textContent).toBe("10004");
  expect(token.getAttribute("aria-invalid")).toBe("true");
  expect(toasts.at(-1)).toEqual({
    kind: "error",
    title: "配置被拒绝",
    body: "机器人不存在",
    code: "422",
  });
  form.setError("token", null);
  expect(error.textContent).toBe("");
});

test("submitJson without onError keeps its plain toast", async () => {
  const { context: ctx, toasts } = context("en");
  const form = settingsForm(
    {
      endpoint: "/api/x",
      submit: "Save",
      rows: [{ title: "A", fields: [{ kind: "text", name: "a", label: "A" }] }],
    },
    ctx,
  );
  form.element.querySelector<HTMLInputElement>('[name="a"]')!.value = "1";
  harness.reply = async () => json(500, { error: "storage", message: "disk" });
  expect(await form.submit()).toBe("failed");
  expect(toasts.at(-1)).toEqual({
    kind: "error",
    title: "Submission failed",
    body: undefined,
    code: "500",
  });
});

test("an error the device can resume says so: retry is read and offered as 重试", async () => {
  expect(
    await deviceError(
      json(502, { error: "upstream_unavailable", retry: true }),
    ),
  ).toEqual({ status: 502, error: "upstream_unavailable", retry: true });
  expect(await deviceError(json(502, { error: "x", retry: "yes" }))).toEqual({
    status: 502,
    error: "x",
  });

  const { context: ctx, toasts } = context("en");
  let retried = 0;
  const retry = () => retried++;
  toastDeviceError(
    ctx,
    { kind: "error", error: { status: 502, error: "x", retry: true } },
    retry,
  );
  expect(toasts.at(-1)?.title).toBe("Submission failed");
  expect(toasts.at(-1)?.action?.label).toBe("Retry");
  toasts.at(-1)?.action?.run();
  expect(retried).toBe(1);
  // without the flag, a refusal is final
  toastDeviceError(
    ctx,
    { kind: "error", error: { status: 422, error: "x" } },
    retry,
  );
  expect(toasts.at(-1)?.action).toBeUndefined();
});

test("readChannel reads the configured flag; configuredRow shows the card once told", async () => {
  const { context: ctx } = context();
  harness.reply = async () =>
    json(200, {
      configured: true,
      signup: { email_address: "a@inkboxmail.com" },
    });
  expect(
    await readChannel<{ signup?: { email_address: string } }>(
      ctx,
      "/api/gateway/inkbox",
    ),
  ).toEqual({
    configured: true,
    signup: { email_address: "a@inkboxmail.com" },
  });
  expect(harness.calls.at(-1)?.method).toBe("GET");
  expect(harness.calls.at(-1)?.url).toBe("/api/gateway/inkbox");
  harness.reply = async () => json(200, { configured: "yes" });
  expect(await readChannel(ctx, "/api/gateway/qq")).toBeNull();
  harness.reply = async () => new Response(null, { status: 405 });
  expect(await readChannel(ctx, "/api/gateway/qq")).toBeNull();

  for (const lang of ["zh", "en"] as const) {
    const current = configuredRow("Telegram", lang);
    expect(current.element.hidden).toBe(true);
    current.show(true);
    expect(current.element.hidden).toBe(false);
    expect(current.element.querySelector(".bc-row__label")?.textContent).toBe(
      lang === "zh" ? "通道" : "Channel",
    );
    expect(current.element.querySelector(".bc-option-title")?.textContent).toBe(
      "Telegram",
    );
    // a stored configuration is not a live connection: a neutral badge
    expect(current.element.querySelector(".bc-badge--signal")).toBeNull();
    expect(current.element.querySelector(".bc-badge")?.textContent).toBe(
      lang === "zh" ? "已配置" : "Configured",
    );
  }
});
