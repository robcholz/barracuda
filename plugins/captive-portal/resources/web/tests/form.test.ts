import { expect, test } from "bun:test";
import { Window } from "happy-dom";
import { configuration } from "../ui/form";

test("invalid fields recover; pending submits are bounded and aborted on removal", async () => {
  const browser = new Window();
  const previous = { document: globalThis.document, fetch: globalThis.fetch };
  Object.assign(globalThis, { document: browser.document });
  let calls = 0;
  let requestSignal: AbortSignal | null | undefined;
  let offline = true;
  globalThis.fetch = Object.assign(
    async (_url: RequestInfo | URL, init?: RequestInit) => {
      calls++;
      if (offline) throw new Error("offline");
      requestSignal = init?.signal;
      return await new Promise<Response>((_resolve, reject) =>
        requestSignal?.addEventListener("abort", () =>
          reject(new Error("abort")),
        ),
      );
    },
    { preconnect: previous.fetch.preconnect },
  );
  const controller = new AbortController();
  const root = browser.document.createElement("div");
  browser.document.body.append(root);
  try {
    configuration({
      title: "Test",
      endpoint: "/api/test",
      description: "Test",
      fields: [
        { name: "url", label: "URL", type: "url" },
        { name: "count", label: "Count", type: "number", value: 1 },
      ],
    })(root as unknown as HTMLElement, { signal: controller.signal });
    const form = root.querySelector("form")!;
    const input = root.querySelector("input")!;
    const submit = () =>
      form.dispatchEvent(new browser.Event("submit", { cancelable: true }));
    input.value = "ftp://bad.example";
    submit();
    expect(calls).toBe(0);
    expect(input.validationMessage).not.toBe("");
    input.value = "https://valid.example";
    input.dispatchEvent(new browser.Event("input"));
    expect(input.validationMessage).toBe("");
    submit();
    await Bun.sleep(1);
    expect(root.textContent).toContain("未收到设备确认");
    offline = false;
    submit();
    submit();
    expect(calls).toBe(2);
    controller.abort();
    await Bun.sleep(1);
    expect(requestSignal?.aborted).toBe(true);
    expect(root.childElementCount).toBe(0);
  } finally {
    controller.abort();
    Object.assign(globalThis, previous);
    await browser.happyDOM.close();
  }
});
