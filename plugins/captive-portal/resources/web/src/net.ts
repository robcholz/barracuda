/**
 * The device's web server holds only a few connections at once, and it refuses (does not queue)
 * any connection beyond them. A browser opens up to six at a time, so a page that loads its
 * icons, figure, module and data together loses some of them at random.
 *
 * The shell therefore sends every request through one queue with a small concurrency limit, and
 * retries a request whose connection was refused. Modules load through the same queue.
 */

/** Requests in flight at once. The server has four slots; Web chat keeps one, so two stay free. */
export const MAX_IN_FLIGHT = 2;
/** Retries after a refused connection, with the delay doubling from `RETRY_MS`. */
export const RETRIES = 4;
export const RETRY_MS = 250;

type Fetch = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

let inFlight = 0;
const waiting: (() => void)[] = [];

async function slot<T>(task: () => Promise<T>): Promise<T> {
  // a finished task hands its slot straight to the next waiter, so none is taken twice
  if (inFlight >= MAX_IN_FLIGHT)
    await new Promise<void>((resolve) => waiting.push(resolve));
  else inFlight++;
  try {
    return await task();
  } finally {
    const next = waiting.shift();
    if (next) next();
    else inFlight--;
  }
}

function pause(ms: number, signal?: AbortSignal | null): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(signal.reason);
      },
      { once: true },
    );
  });
}

function method(input: RequestInfo | URL, init?: RequestInit): string {
  return (
    init?.method ??
    (typeof Request !== "undefined" && input instanceof Request
      ? input.method
      : "GET")
  ).toUpperCase();
}

/**
 * Wraps `fetch` in the queue. A refused connection surfaces as a `TypeError`; a GET or HEAD is
 * retried then, since the request never reached the server. Other methods fail as before.
 */
export function queuedFetch(native: Fetch): Fetch {
  return async (input, init) => {
    const retry = ["GET", "HEAD"].includes(method(input, init));
    for (let attempt = 0; ; attempt++) {
      try {
        return await slot(() => native(input, init));
      } catch (error) {
        if (
          !retry ||
          !(error instanceof TypeError) ||
          init?.signal?.aborted ||
          attempt >= RETRIES
        )
          throw error;
      }
      await pause(RETRY_MS * 2 ** attempt, init?.signal);
    }
  };
}

/**
 * Imports a module through the queue, retrying a failed load. A retry adds `?r=<n>` so the
 * browser fetches again instead of reusing the failed attempt; the server ignores the query.
 */
export function queuedImport(
  load: (url: string) => Promise<unknown> = (url) => import(url),
): (url: string) => Promise<unknown> {
  return async (url) => {
    for (let attempt = 0; ; attempt++) {
      const target =
        attempt === 0
          ? url
          : `${url}${url.includes("?") ? "&" : "?"}r=${attempt}`;
      try {
        return await slot(() => load(target));
      } catch (error) {
        if (attempt >= RETRIES) throw error;
      }
      await pause(RETRY_MS * 2 ** attempt);
    }
  };
}

/** Routes the window's `fetch` through the queue, so every page's requests share it. */
export function installQueuedFetch(win: Window & typeof globalThis) {
  win.fetch = queuedFetch(win.fetch.bind(win)) as typeof fetch;
}
