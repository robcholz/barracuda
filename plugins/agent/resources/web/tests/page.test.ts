import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  installBrowser,
  settle,
} from "../../../../captive-portal/resources/web/tests/browser";
import type {
  Lang,
  PortalContext,
  Toast,
} from "../../../../captive-portal/resources/web/ui";
import { mount } from "../entry";

let browser: ReturnType<typeof installBrowser>;
let calls: [string, RequestInit][];
let respond: () => Promise<Response>;
const realFetch = globalThis.fetch;

beforeEach(() => {
  browser = installBrowser();
  calls = [];
  respond = async () => new Response(null, { status: 204 });
  globalThis.fetch = Object.assign(
    async (url: RequestInfo | URL, init: RequestInit = {}) => {
      calls.push([String(url), init]);
      return respond();
    },
    { preconnect: realFetch.preconnect },
  );
});
afterEach(async () => {
  globalThis.fetch = realFetch;
  await browser.close();
});

async function render(lang: Lang = "zh") {
  const controller = new AbortController();
  const toasts: Toast[] = [];
  const routes: string[] = [];
  const refreshes = { count: 0 };
  const context: PortalContext = {
    signal: controller.signal,
    lang,
    toast: (toast) => {
      if (!controller.signal.aborted) toasts.push(toast);
    },
    navigate: (id) => routes.push(id),
    status: () => null,
    refreshStatus: async () => {
      refreshes.count++;
    },
  };
  const root = browser.document.createElement("div");
  browser.document.body.append(root);
  const cleanup = await mount(root, context);
  const form = root.querySelector("form")!;
  return {
    root,
    form,
    toasts,
    routes,
    refreshes,
    controller,
    cleanup: cleanup as () => void,
    input: (name: string) =>
      root.querySelector<HTMLInputElement>(`[name="${name}"]`)!,
    submit: async () => {
      form.dispatchEvent(
        new browser.window.Event("submit", { cancelable: true }),
      );
      await settle();
    },
  };
}

const texts = (root: ParentNode, selector: string) =>
  [...root.querySelectorAll(selector)].map((node) => node.textContent);

test("renders the design in Chinese: header, socket figure, purpose cards, fold, footer", async () => {
  const { root, input } = await render("zh");
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("模型配置");
  expect(root.querySelector(".bc-header h1 .bc-asset-icon")).toBeNull();
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "为每种 Agent 用途注册模型。",
  );
  const figure = root.querySelector<HTMLElement>(
    ".bc-header__figure hl-figure",
  );
  expect(figure?.getAttribute("name")).toBe("agent");
  expect(figure?.style.maxWidth).toBe("300px");
  expect(texts(root, ".bc-row__label .bc-title")).toEqual([
    "接口协议",
    "用途",
    "连接",
  ]);
  expect(texts(root, ".bc-radio-card .bc-option-title")).toEqual([
    "OpenAI 兼容",
    "Anthropic 兼容",
    "主 Agent",
    "子 Agent",
    "记忆",
    "压缩",
  ]);
  expect(
    root.querySelectorAll(".bc-radio-card--icon .bc-option-icon svg"),
  ).toHaveLength(6);
  expect(
    root.querySelector<HTMLInputElement>('[name="backend"]:checked')?.value,
  ).toBe("openai_compatible");
  expect(
    root.querySelector<HTMLInputElement>('[name="purpose"]:checked')?.value,
  ).toBe("root_agent");
  const toggle = root.querySelector('[role="switch"]')!;
  expect(toggle.getAttribute("aria-checked")).toBe("true");
  expect(root.querySelector(".bc-switch-row")?.textContent).toBe(
    "设为默认模型该用途优先使用这个模型",
  );
  expect(input("base_url").placeholder).toBe("https://api.example.com/v1");
  expect(input("model").placeholder).toBe("服务商使用的模型 ID");
  expect(input("api_key").type).toBe("password");
  expect(
    root.querySelector(".bc-disclosure")?.getAttribute("aria-expanded"),
  ).toBe("true");
  expect(texts(root, ".bc-addon")).toEqual(["ms", "tokens", "bytes"]);
  expect(input("timeout_ms").value).toBe("120000");
  expect(input("max_tokens").value).toBe("8192");
  expect(input("image_max_bytes").value).toBe("524288");
  expect(root.textContent).toContain("超时与大小上限");
  expect(root.textContent).not.toContain("/api/");
  expect(texts(root, ".bc-form__footer button")).toEqual(["清空", "注册模型"]);
});

test("renders English copy from the context language", async () => {
  const { root } = await render("en");
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("Models");
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "Register a model for each agent purpose.",
  );
  expect(texts(root, ".bc-row__label .bc-title")).toEqual([
    "API format",
    "Purpose",
    "Connection",
  ]);
  expect(texts(root, ".bc-radio-card .bc-hint")).toEqual([
    "Chat Completions; works with most providers",
    "Messages API",
    "Replies to the user",
    "Runs delegated tasks",
    "Organizes long-term memory",
    "Compacts the conversation context",
  ]);
  expect(root.textContent).toContain("Make default");
  expect(root.textContent).toContain("Timeouts and size limits");
  expect(texts(root, ".bc-form__footer button")).toEqual([
    "Clear",
    "Register model",
  ]);
});

test("required connection fields block the request", async () => {
  const { root, input, submit } = await render("zh");
  input("base_url").value = "api.example.com";
  await submit();
  expect(calls).toHaveLength(0);
  expect(input("base_url").getAttribute("aria-invalid")).toBe("true");
  expect(input("model").getAttribute("aria-invalid")).toBe("true");
  expect(input("api_key").getAttribute("aria-invalid")).toBe("true");
  expect(root.textContent).toContain(
    "请输入以 http:// 或 https:// 开头的地址。",
  );
  expect(root.textContent).toContain("请填写 模型名称。");
  expect(root.textContent).toContain("请填写 API Key。");
});

test("posts one model as a batch with every field the endpoint requires", async () => {
  const { root, input, submit, toasts, routes, refreshes } = await render("zh");
  root
    .querySelector<HTMLInputElement>('[value="anthropic_compatible"]')!
    .click();
  root.querySelector<HTMLInputElement>('[value="memory"]')!.click();
  root.querySelector<HTMLButtonElement>('[role="switch"]')!.click();
  input("base_url").value = " https://api.example.com/v1 ";
  input("model").value = "claude-model";
  input("api_key").value = "sk-secret";
  input("max_tokens").value = "4096";
  await submit();
  expect(calls).toHaveLength(1);
  expect(calls[0][0]).toBe("/api/model-api");
  expect(calls[0][1].method).toBe("POST");
  expect(JSON.parse(String(calls[0][1].body))).toEqual([
    {
      backend: "anthropic_compatible",
      purpose: "memory",
      default: false,
      base_url: "https://api.example.com/v1",
      model: "claude-model",
      api_key: "sk-secret",
      timeout_ms: 120000,
      max_tokens: 4096,
      image_max_bytes: 524288,
    },
  ]);
  expect(toasts).toHaveLength(1);
  expect(toasts[0].kind).toBe("success");
  expect(toasts[0].title).toBe("设备已接受配置");
  expect(toasts[0].action?.label).toBe("去 Web 聊天试试");
  toasts[0].action?.run();
  expect(routes).toEqual(["imessage-web"]);
  expect(input("api_key").value).toBe("");
  // the portal reads the status again, so 注册模型 shows as done
  expect(refreshes.count).toBe(1);
});

test("a rejected configuration toasts the status and keeps the key", async () => {
  const { input, submit, toasts, refreshes } = await render("en");
  input("base_url").value = "https://api.example.com/v1";
  input("model").value = "m";
  input("api_key").value = "sk-secret";
  respond = async () => new Response(null, { status: 422 });
  await submit();
  expect(toasts).toEqual([
    { kind: "error", title: "Configuration rejected", code: "422" },
  ]);
  expect(input("api_key").value).toBe("sk-secret");
  expect(refreshes.count).toBe(0);
});

test("leaving the page clears the key and empties the root", async () => {
  const { root, input, controller, cleanup } = await render("zh");
  input("api_key").value = "sk-secret";
  controller.abort();
  expect(input("api_key").value).toBe("");
  cleanup();
  expect(root.childElementCount).toBe(0);
});
