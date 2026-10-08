import { afterEach, beforeEach, expect, test } from "bun:test";
import { installBrowser, settle } from "./browser";
import {
  MARK_OPENAI,
  definePage,
  header,
  kv,
  page,
  settingsForm,
  submitJson,
  type PortalContext,
  type SettingsFormOptions,
  type Toast,
} from "../ui";

let browser: ReturnType<typeof installBrowser>;
let calls: [string, RequestInit][];
let respond: (init: RequestInit) => Promise<Response>;
const realFetch = globalThis.fetch;

beforeEach(() => {
  browser = installBrowser();
  calls = [];
  respond = async () => new Response(null, { status: 204 });
  globalThis.fetch = Object.assign(
    async (url: RequestInfo | URL, init: RequestInit = {}) => {
      calls.push([String(url), init]);
      return respond(init);
    },
    { preconnect: realFetch.preconnect },
  );
});
afterEach(async () => {
  globalThis.fetch = realFetch;
  await browser.close();
});

function context(lang: "zh" | "en" = "zh") {
  const controller = new AbortController();
  const toasts: Toast[] = [];
  const routes: string[] = [];
  const value: PortalContext = {
    signal: controller.signal,
    lang,
    toast: (toast) => {
      if (!controller.signal.aborted) toasts.push(toast);
    },
    navigate: (id) => routes.push(id),
    status: () => null,
    refreshStatus: async () => {},
  };
  return { context: value, controller, toasts, routes };
}

const OPTIONS: SettingsFormOptions = {
  endpoint: "/api/gateway/telegram",
  submit: { zh: "保存并替换通道", en: "Save and replace channel" },
  rows: [
    {
      title: "Bot",
      hint: {
        zh: "在 @BotFather 创建 Bot 后获得 Token",
        en: "From @BotFather",
      },
      fields: [{ kind: "secret", name: "token", label: "Bot Token" }],
    },
    {
      title: { zh: "选项", en: "Options" },
      fields: [
        {
          kind: "switch",
          name: "use_private_api",
          label: { zh: "使用 Private API", en: "Use the Private API" },
          value: true,
        },
        {
          kind: "radio",
          name: "backend",
          label: { zh: "接口协议", en: "API format" },
          options: [
            { value: "openai_compatible", label: "OpenAI", icon: MARK_OPENAI },
            { value: "anthropic_compatible", label: "Anthropic" },
          ],
        },
        {
          kind: "select",
          name: "purpose",
          label: "Purpose",
          options: [
            { value: "root_agent", label: "Main" },
            { value: "memory", label: "Memory" },
          ],
          value: "memory",
        },
      ],
    },
  ],
  advanced: {
    fields: [
      {
        kind: "url",
        name: "api_base",
        label: "API Base URL",
        value: "https://api.telegram.org",
      },
      {
        kind: "number",
        name: "draft_min_delta_bytes",
        label: { zh: "草稿最小增量", en: "Draft minimum delta" },
        value: 24,
        unit: "bytes",
      },
      {
        kind: "text",
        name: "route_tag",
        label: { zh: "路由标签", en: "Route tag" },
        optional: true,
      },
    ],
  },
};

const input = (root: ParentNode, name: string) =>
  root.querySelector<HTMLInputElement>(`[name="${name}"]`)!;
const submit = (form: HTMLFormElement) =>
  form.dispatchEvent(new browser.window.Event("submit", { cancelable: true }));

test("the settings form follows the design: rows, secret toggle, fold, footer", () => {
  const { context: ctx } = context();
  const { element } = settingsForm(OPTIONS, ctx);
  expect(element.classList.contains("bc-form")).toBe(true);
  expect(
    [...element.querySelectorAll(".bc-row__label .bc-title")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["Bot", "选项"]);
  expect(element.querySelector(".bc-form__endpoint")?.textContent).toBe(
    "POST /api/gateway/telegram · 只写设备不会返回已保存的设置和密钥",
  );
  expect(
    [...element.querySelectorAll(".bc-form__footer button")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["清空", "保存并替换通道"]);
  const token = input(element, "token");
  expect(token.type).toBe("password");
  expect(token.autocomplete).toBe("new-password");
  const reveal = element.querySelector<HTMLButtonElement>(
    `[aria-controls="${token.id}"]`,
  )!;
  expect(reveal.textContent).toBe("显示");
  reveal.click();
  expect(token.type).toBe("text");
  expect(reveal.textContent).toBe("隐藏");
  expect(reveal.getAttribute("aria-pressed")).toBe("true");
  const disclosure =
    element.querySelector<HTMLButtonElement>(".bc-disclosure")!;
  expect(disclosure.getAttribute("aria-expanded")).toBe("false");
  disclosure.click();
  expect(disclosure.getAttribute("aria-expanded")).toBe("true");
  expect(element.querySelector(".bc-addon")?.textContent).toBe("bytes");
  expect(
    element.querySelector(".bc-radio-card--icon .bc-option-icon svg"),
  ).not.toBeNull();
  expect(element.textContent).toContain("（可选）");
});

test("invalid fields block the request, are marked red with a message, and recover on input", async () => {
  const { context: ctx } = context();
  const form = settingsForm(OPTIONS, ctx);
  const root = form.element;
  browser.document.body.append(root);
  input(root, "api_base").value = "ftp://bad.example";
  input(root, "draft_min_delta_bytes").value = "-1";
  submit(root);
  await settle();
  expect(calls).toHaveLength(0);
  const token = input(root, "token");
  expect(token.getAttribute("aria-invalid")).toBe("true");
  const error = root.querySelector(`#${token.id}-error`)!;
  expect(error.textContent).toBe("请填写 Bot Token。");
  expect((error as HTMLElement).hidden).toBe(false);
  expect(input(root, "api_base").getAttribute("aria-invalid")).toBe("true");
  expect(root.textContent).toContain(
    "请输入以 http:// 或 https:// 开头的地址。",
  );
  expect(root.textContent).toContain("请输入 0 到 4294967295 之间的整数。");
  // the fold opened to show the invalid defaults
  expect(
    root.querySelector(".bc-disclosure")?.getAttribute("aria-expanded"),
  ).toBe("true");
  token.value = "123:abc";
  token.dispatchEvent(new browser.window.Event("input"));
  expect(token.hasAttribute("aria-invalid")).toBe(false);
  expect((error as HTMLElement).hidden).toBe(true);
});

test("submit posts JSON, toasts the outcome and clears secrets once accepted", async () => {
  const { context: ctx, toasts } = context();
  let accepted: unknown;
  const form = settingsForm(
    {
      ...OPTIONS,
      onSuccess: (values) => (accepted = values),
      success: {
        action: {
          label: { zh: "去 Web 聊天试试", en: "Try it in Web chat" },
          run: () => ctx.navigate("imessage-web"),
        },
      },
    },
    ctx,
  );
  const root = form.element;
  input(root, "token").value = " secret ";
  respond = async () => new Response(null, { status: 422 });
  expect(await form.submit()).toBe("rejected");
  expect(calls[0][0]).toBe("/api/gateway/telegram");
  expect(calls[0][1].method).toBe("POST");
  expect(calls[0][1].redirect).toBe("error");
  expect((calls[0][1].headers as Record<string, string>)["Content-Type"]).toBe(
    "application/json",
  );
  expect(JSON.parse(String(calls[0][1].body))).toEqual({
    token: " secret ",
    use_private_api: true,
    backend: "openai_compatible",
    purpose: "memory",
    api_base: "https://api.telegram.org",
    draft_min_delta_bytes: 24,
  });
  expect(toasts[0]).toEqual({
    kind: "error",
    title: "配置被拒绝",
    code: "422",
  });
  expect(input(root, "token").value).toBe(" secret ");

  respond = async () => new Response(null, { status: 204 });
  expect(await form.submit()).toBe("accepted");
  expect(toasts[1].kind).toBe("success");
  expect(toasts[1].title).toBe("设备已接受配置");
  expect(toasts[1].action?.label).toBe("去 Web 聊天试试");
  expect(input(root, "token").value).toBe("");
  expect(accepted).toMatchObject({ token: " secret " });

  input(root, "token").value = "x";
  respond = async () => {
    throw new Error("offline");
  };
  expect(await form.submit()).toBe("failed");
  expect(toasts[2]).toMatchObject({
    kind: "error",
    title: "未收到设备确认",
    body: "配置可能已生效。",
  });
  respond = async () => new Response(null, { status: 204 });
  toasts[2].action?.run();
  await settle();
  expect(calls).toHaveLength(4);
  expect(toasts[3].kind).toBe("success");
});

test("one request at a time; leaving the page aborts it silently and clears secrets", async () => {
  const { context: ctx, controller, toasts } = context("en");
  const form = settingsForm(OPTIONS, ctx);
  input(form.element, "token").value = "secret";
  let signal: AbortSignal | null | undefined;
  respond = (init) =>
    new Promise((_resolve, reject) => {
      signal = init.signal;
      init.signal?.addEventListener("abort", () =>
        reject(new Error("aborted")),
      );
    });
  const first = form.submit();
  const second = form.submit();
  expect(first).toBe(second);
  expect(calls).toHaveLength(1);
  expect(
    form.element.querySelector<HTMLButtonElement>("[type=submit]")?.disabled,
  ).toBe(true);
  controller.abort();
  expect(await first).toBe("aborted");
  expect(signal?.aborted).toBe(true);
  expect(toasts).toHaveLength(0);
  expect(input(form.element, "token").value).toBe("");
});

test("clear restores defaults; English strings follow the context language", () => {
  const { context: ctx } = context("en");
  const form = settingsForm(OPTIONS, ctx);
  const root = form.element;
  input(root, "api_base").value = "https://other.example";
  input(root, "token").value = "t";
  (root.querySelector('[role="switch"]') as HTMLButtonElement).click();
  expect(form.values()?.use_private_api).toBe(false);
  root.dispatchEvent(new browser.window.Event("reset", { cancelable: true }));
  expect(input(root, "api_base").value).toBe("https://api.telegram.org");
  expect(input(root, "token").value).toBe("");
  expect(
    root.querySelector('[role="switch"]')?.getAttribute("aria-checked"),
  ).toBe("true");
  expect(form.values()).toBeNull();
  expect(root.textContent).toContain("Enter the Bot Token.");
  expect(root.querySelector(".bc-disclosure")?.textContent).toBe("Advanced");
  expect(root.textContent).toContain("write-only");
  form.setError("route_tag", "Taken");
  expect(input(root, "route_tag").getAttribute("aria-invalid")).toBe("true");
});

test("header, kv and definePage build the page frame and clean up", async () => {
  const { context: ctx, controller } = context();
  const mount = definePage(({ lang }) =>
    page(
      header(
        {
          title: "Telegram",
          lead: {
            zh: "通过 Telegram Bot 收发消息。",
            en: "Send and receive through a bot.",
          },
          icon: "/portal/assets/imessage-telegram/icon.svg",
          figure: "riffle",
          figureWidth: 280,
          extra: kv([["状态", "—"]], lang, { live: true }),
        },
        lang,
      ),
    ),
  );
  const root = browser.document.createElement("div");
  const cleanup = await mount(root, ctx);
  expect(root.querySelector(".bc-page .bc-header h1")?.textContent).toBe(
    "Telegram",
  );
  expect(root.querySelector(".bc-header .bc-asset-icon")).not.toBeNull();
  expect(
    root.querySelector(".bc-header__figure hl-figure")?.getAttribute("name"),
  ).toBe("riffle");
  expect(root.querySelector(".bc-kv")?.getAttribute("role")).toBe("status");
  controller.abort();
  (cleanup as () => void)();
  expect(root.childElementCount).toBe(0);
});

test("submitJson reports 404 and 5xx distinctly", async () => {
  const { context: ctx, toasts } = context();
  respond = async () => new Response(null, { status: 404 });
  expect(await submitJson(ctx, { endpoint: "/api/x", body: {} })).toBe(
    "rejected",
  );
  respond = async () => new Response(null, { status: 503 });
  expect(
    await submitJson(ctx, { endpoint: "/api/x", body: [], method: "PUT" }),
  ).toBe("failed");
  expect(toasts.map((toast) => [toast.title, toast.code])).toEqual([
    ["接口不可用", "404"],
    ["提交失败", "503"],
  ]);
  expect(calls[1][1].method).toBe("PUT");
});
