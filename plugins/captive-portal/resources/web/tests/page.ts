import { installBrowser, settle } from "./browser";
import type {
  EntryStatus,
  Lang,
  PortalContext,
  PortalModule,
  Toast,
} from "../ui";

export interface FetchCall {
  url: string;
  method: string;
  body: unknown;
  init: RequestInit;
}

/**
 * A happy-dom window plus a scripted `fetch`: `reply(call)` answers each request (default 204), and
 * every call is recorded with its method and parsed JSON body. Call `close()` in `afterEach`.
 */
export function pageHarness() {
  const browser = installBrowser();
  const realFetch = globalThis.fetch;
  const calls: FetchCall[] = [];
  const harness = {
    browser,
    calls,
    reply: async (_call: FetchCall): Promise<Response> =>
      new Response(null, { status: 204 }),
    /** What `context.status(id)` answers. */
    statuses: new Map<string, EntryStatus>(),
    /** Mounts `mount` in a fresh root with a recording context. */
    async render(mount: PortalModule["mount"], lang: Lang = "zh") {
      const controller = new AbortController();
      const toasts: Toast[] = [];
      const routes: string[] = [];
      /** How many times the page asked the shell to read the status again. */
      const refreshes = { count: 0 };
      const context: PortalContext = {
        signal: controller.signal,
        lang,
        toast: (toast) => {
          if (!controller.signal.aborted) toasts.push(toast);
        },
        navigate: (id) => routes.push(id),
        status: (id = "page") => harness.statuses.get(id) ?? null,
        refreshStatus: async () => {
          if (!controller.signal.aborted) refreshes.count++;
        },
      };
      const root = browser.document.createElement("div");
      browser.document.body.append(root);
      const cleanup = await mount(root, context);
      const query = <T extends Element = HTMLElement>(selector: string) =>
        root.querySelector<T>(selector);
      return {
        root,
        context,
        toasts,
        routes,
        refreshes,
        query,
        input: (name: string) =>
          root.querySelector<HTMLInputElement>(`[name="${name}"]`)!,
        /** Sets an input's value as typing does (fires `input`). */
        type(name: string, value: string) {
          const input = root.querySelector<HTMLInputElement>(
            `[name="${name}"]`,
          )!;
          input.value = value;
          input.dispatchEvent(
            new browser.window.Event("input", { bubbles: true }),
          );
        },
        /** Clicks the first button whose text is `label`. */
        async click(label: string) {
          const target = [
            ...root.querySelectorAll<HTMLElement>("button, a"),
          ].find((node) => node.textContent?.trim() === label);
          if (!target) throw new Error(`no button «${label}»`);
          target.click();
          await settle();
        },
        async submit() {
          root
            .querySelector("form")!
            .dispatchEvent(
              new browser.window.Event("submit", { cancelable: true }),
            );
          await settle();
        },
        /** Aborts the signal and runs the cleanup, as the shell does on navigation. */
        unmount() {
          controller.abort();
          if (typeof cleanup === "function") cleanup();
        },
        text: () => root.textContent ?? "",
      };
    },
    async close() {
      globalThis.fetch = realFetch;
      await browser.close();
    },
  };
  globalThis.fetch = Object.assign(
    async (url: RequestInfo | URL, init: RequestInit = {}) => {
      // the kit fetches its page's icon itself; that is no call the page makes
      if (/\.(svg|png)$/.test(String(url)))
        return new Response(null, { status: 404 });
      const raw = init.body;
      let body: unknown = raw;
      if (typeof raw === "string")
        try {
          body = JSON.parse(raw);
        } catch {
          body = raw;
        }
      const call: FetchCall = {
        url: String(url),
        method: init.method ?? "GET",
        body,
        init,
      };
      calls.push(call);
      return harness.reply(call);
    },
    { preconnect: realFetch.preconnect },
  );
  return harness;
}

/** A JSON response. */
export const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

export { settle };
