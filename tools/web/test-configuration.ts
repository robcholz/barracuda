import { expect, test } from "bun:test";
import { Window, type HTMLInputElement } from "happy-dom";

/** Common test harness; each plugin owns its endpoint and payload assertions. */
export function testConfiguration(
  id: string,
  mount: (
    root: HTMLElement,
    context: { signal: AbortSignal },
  ) => void | (() => void) | Promise<void | (() => void)>,
  endpoint: string,
  values: Record<string, string>,
  expected: unknown,
) {
  test(`${id}: submits only on demand, handles failure and clears secrets`, async () => {
    const browser = new Window({ url: "http://localhost/portal/" });
    const oldDocument = globalThis.document;
    const oldFetch = globalThis.fetch;
    Object.assign(globalThis, { document: browser.document });
    const root = browser.document.createElement("div");
    browser.document.body.append(root);
    const controller = new AbortController();
    const calls: [string, RequestInit][] = [];
    let status = 422;
    globalThis.fetch = Object.assign(
      async (url: RequestInfo | URL, init: RequestInit = {}) => {
        calls.push([String(url), init]);
        return new Response(null, { status });
      },
      { preconnect: oldFetch.preconnect },
    );
    try {
      const cleanup = await mount(root as unknown as HTMLElement, {
        signal: controller.signal,
      });
      expect(calls).toHaveLength(0);
      expect(root.textContent).toContain("不读取");
      for (const [name, value] of Object.entries(values))
        root.querySelector<HTMLInputElement>(`[name=${name}]`)!.value = value;
      const submit = () =>
        root
          .querySelector("form")!
          .dispatchEvent(new browser.Event("submit", { cancelable: true }));
      submit();
      await Bun.sleep(1);
      expect(calls[0][0]).toBe(endpoint);
      expect(calls[0][1].method).toBe("POST");
      const body = JSON.parse(String(calls[0][1].body));
      expect(body).toEqual(expected);
      expect(calls[0][1].redirect).toBe("error");
      expect(root.textContent).toContain("配置被拒绝");
      status = 204;
      submit();
      await Bun.sleep(1);
      expect(root.textContent).toContain("已接受");
      for (const input of root.querySelectorAll<HTMLInputElement>(
        "input[type=password]",
      ))
        expect(input.value).toBe("");
      controller.abort();
      if (cleanup) cleanup();
      expect(root.childElementCount).toBe(0);
    } finally {
      controller.abort();
      Object.assign(globalThis, { document: oldDocument, fetch: oldFetch });
      await browser.happyDOM.close();
    }
  });
}
