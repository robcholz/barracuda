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
  // the mount reads what the device holds; set that read aside
  await settle();
  expect(calls.map(([url, init]) => [url, init.method])).toEqual([
    ["/api/tavily/status", "GET"],
  ]);
  calls.length = 0;
  const form = root.querySelector("form")!;
  return {
    root,
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

test("renders the design in Chinese: untitled-icon header, loupe figure, key row, open fold", async () => {
  const { root, input } = await render("zh");
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("网页搜索");
  expect(root.querySelector(".bc-header h1")?.childElementCount).toBe(0);
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "让 Agent 通过 Tavily 搜索网页。",
  );
  const figure = root.querySelector<HTMLElement>(
    ".bc-header__figure hl-figure",
  );
  expect(figure?.getAttribute("name")).toBe("agent-websearch");
  expect(figure?.style.maxWidth).toBe("280px");
  expect(texts(root, ".bc-row__label .bc-title")).toEqual(["Tavily"]);
  expect(root.textContent).toContain("在 Tavily 控制台创建 API Key");
  expect(root.textContent).toContain("Tavily API Key");
  expect(input("api_key").type).toBe("password");
  expect(
    root.querySelector(".bc-disclosure")?.getAttribute("aria-expanded"),
  ).toBe("true");
  expect(root.textContent).toContain("已填入默认值");
  expect(input("api_base").value).toBe("https://api.tavily.com");
  expect(root.textContent).not.toContain("/api/");
  expect(texts(root, ".bc-form__footer button")).toEqual(["清空", "保存配置"]);
});

test("renders English copy from the context language", async () => {
  const { root } = await render("en");
  expect(root.querySelector(".bc-header h1")?.textContent).toBe("Web search");
  expect(root.querySelector(".bc-lead")?.textContent).toBe(
    "Let the agent search the web through Tavily.",
  );
  expect(root.textContent).toContain(
    "Create an API key in the Tavily dashboard",
  );
  expect(root.textContent).toContain("Defaults filled in");
  expect(texts(root, ".bc-form__footer button")).toEqual(["Clear", "Save"]);
});

test("the key is required and the API base must be http(s)", async () => {
  const { root, input, submit } = await render("en");
  input("api_base").value = "api.tavily.com";
  await submit();
  expect(calls).toHaveLength(0);
  expect(input("api_key").getAttribute("aria-invalid")).toBe("true");
  expect(input("api_base").getAttribute("aria-invalid")).toBe("true");
  expect(root.textContent).toContain("Enter the Tavily API Key.");
  expect(root.textContent).toContain(
    "Enter an address starting with http:// or https://.",
  );
});

test("posts the key and API base, toasts success and clears the key", async () => {
  const { input, submit, toasts, routes, refreshes } = await render("zh");
  input("api_key").value = "tvly-secret";
  await submit();
  // the accepted save reads the device's configuration again
  expect(calls.map(([url]) => url)).toEqual([
    "/api/tavily",
    "/api/tavily/status",
  ]);
  expect(calls[0][1].method).toBe("POST");
  expect(JSON.parse(String(calls[0][1].body))).toEqual({
    api_key: "tvly-secret",
    api_base: "https://api.tavily.com",
  });
  expect(toasts).toHaveLength(1);
  expect(toasts[0].kind).toBe("success");
  expect(toasts[0].title).toBe("设备已接受配置");
  toasts[0].action?.run();
  expect(routes).toEqual(["imessage-web"]);
  expect(input("api_key").value).toBe("");
  expect(refreshes.count).toBe(1);
});

test("the header shows the API base the device holds", async () => {
  respond = async () =>
    Response.json({
      configured: true,
      config: { api_base: "https://tavily.example" },
    });
  const { root } = await render("en");
  expect(root.querySelector(".bc-kv")?.textContent).toBe(
    "Status—API Basehttps://tavily.example",
  );
});

test("a rejected key toasts the status and stays in the field", async () => {
  const { input, submit, toasts, refreshes } = await render("zh");
  input("api_key").value = "tvly-secret";
  respond = async () => new Response(null, { status: 400 });
  await submit();
  expect(toasts).toEqual([{ kind: "error", title: "配置被拒绝", code: "400" }]);
  expect(input("api_key").value).toBe("tvly-secret");
  expect(refreshes.count).toBe(0);
});

test("leaving the page clears the key and empties the root", async () => {
  const { root, input, controller, cleanup } = await render("zh");
  input("api_key").value = "tvly-secret";
  controller.abort();
  expect(input("api_key").value).toBe("");
  cleanup();
  expect(root.childElementCount).toBe(0);
});
