import { expect, test, spyOn } from "bun:test";
import { Window } from "happy-dom";

test("shell renders real entries safely, handles import errors, removal and reconnect", async () => {
  const browser = new Window({ url: "http://localhost/portal/" });
  browser.document.write(
    await Bun.file(new URL("../src/index.html", import.meta.url)).text(),
  );
  const originalFetch = globalThis.fetch;
  const originalWindow = globalThis.window;
  const originalDocument = globalThis.document;
  Object.assign(globalThis, { window: browser, document: browser.document });
  let records = [
    {
      id: "wifi",
      title: '<img src=x onerror="alert(1)">',
      module: "/portal/assets/wifi/entry.js",
    },
  ];
  let offline = false;
  const errors = spyOn(console, "error").mockImplementation(() => {});
  globalThis.fetch = Object.assign(
    async () => {
      if (offline) throw new Error("offline fixture");
      return Response.json(records);
    },
    { preconnect: originalFetch.preconnect },
  );
  const settle = () => Bun.sleep(20);
  try {
    await import("../src/app");
    await settle();
    const document = browser.document;
    expect(document.querySelectorAll(".card")).toHaveLength(1);
    expect(document.querySelector(".card h3")?.textContent).toBe(
      records[0].title,
    );
    expect(document.querySelectorAll("img")).toHaveLength(0);
    (document.querySelector(".card") as unknown as HTMLButtonElement).click();
    await settle();
    expect(document.querySelector("#module-panel")?.textContent).toContain(
      "模块加载失败",
    );
    (
      document.querySelector(
        "#module-panel button",
      ) as unknown as HTMLButtonElement
    ).click();
    await settle();
    records = [];
    (
      document.querySelector("#refresh") as unknown as HTMLButtonElement
    ).click();
    await settle();
    expect(document.querySelectorAll(".card")).toHaveLength(0);
    expect(document.querySelectorAll(".nav-item")).toHaveLength(1);
    expect(document.querySelector("#notice")?.textContent).toContain("已停用");
    expect(
      document.querySelector("#module-panel")?.hasAttribute("hidden"),
    ).toBe(true);
    offline = true;
    (
      document.querySelector("#refresh") as unknown as HTMLButtonElement
    ).click();
    await settle();
    expect(document.querySelector("#connection")?.textContent).toBe(
      "连接未就绪",
    );
    offline = false;
    (
      document.querySelector("#refresh") as unknown as HTMLButtonElement
    ).click();
    await settle();
    expect(document.querySelector("#connection")?.textContent).toBe(
      "设备已连接",
    );
    (document.querySelector(".brand") as unknown as HTMLAnchorElement).click();
    (
      document.querySelector(".nav-item") as unknown as HTMLButtonElement
    ).click();
    browser.document.dispatchEvent(new browser.Event("visibilitychange"));
    await settle();
    expect(errors).toHaveBeenCalled();
  } finally {
    browser.dispatchEvent(new browser.Event("pagehide"));
    globalThis.fetch = originalFetch;
    errors.mockRestore();
    Object.assign(globalThis, {
      window: originalWindow,
      document: originalDocument,
    });
    await browser.happyDOM.close();
  }
});
