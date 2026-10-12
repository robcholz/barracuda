import { Window as HappyWindow } from "happy-dom";

/** A happy-dom window installed as the globals the shell, the kit and the figure kernel read. */
export function installBrowser(url = "http://localhost/portal/") {
  const browser = new HappyWindow({ url, width: 1280, height: 900 });
  const keys = [
    "window",
    "document",
    "matchMedia",
    "IntersectionObserver",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "localStorage",
    "HTMLElement",
    "customElements",
    "PointerEvent",
    "Event",
  ] as const;
  const previous = Object.fromEntries(
    keys.map((key) => [key, (globalThis as Record<string, unknown>)[key]]),
  );
  const own = browser as unknown as Record<string, unknown>;
  Object.assign(
    globalThis,
    Object.fromEntries(
      keys.map((key) => {
        const value = key === "window" ? browser : own[key];
        return [
          key,
          typeof value === "function" && /^[a-z]/.test(key)
            ? (value as (...args: unknown[]) => unknown).bind(browser)
            : value,
        ];
      }),
    ),
  );
  return {
    browser,
    window: browser as unknown as Window & typeof globalThis,
    document: browser.document as unknown as Document,
    async close() {
      Object.assign(globalThis, previous);
      await browser.happyDOM.close();
    },
  };
}

export const settle = (ms = 5) => Bun.sleep(ms);
