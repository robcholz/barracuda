/**
 * Sends an app to a device through its portal (docs/plugins/files.mdx, Live Editor): an HTTPS
 * page may not call `http://` addresses on the local network, so Live Editor opens the device's
 * `/portal/#files` in a window and hands it the app's files with `postMessage`. The person
 * installs it from that window, which writes `/workspace/media/apps/<id>.bapp/`.
 */

export const PROTOCOL = 1;

/** Where a handoff is, as the Install page's steps show it. */
export type Phase =
  "idle" | "opening" | "failed" | "closed" | "ready" | "writing" | "done";

export type Failure =
  | { kind: "blocked" }
  | { kind: "silent" }
  | { kind: "refused" }
  | { kind: "device"; message: string };

/** An app as the portal takes it: its manifest's fields and its container's files. */
export interface Offer {
  id: string;
  name: string;
  version: string;
  files: ReadonlyMap<string, Uint8Array>;
}

/** A device portal's origin from what the person typed: a host, with a port when it has one. */
export function portalOrigin(address: string): string | null {
  const trimmed = address.trim().replace(/\/+$/, "");
  if (!trimmed) return null;
  try {
    const url = new URL(
      /^https?:\/\//.test(trimmed) ? trimmed : `http://${trimmed}`,
    );
    if (url.pathname !== "/" || url.search || url.hash || url.username)
      return null;
    return url.origin;
  } catch {
    return null;
  }
}

/** The portal's Files page at `origin`, which takes the app. */
export function portalUrl(origin: string): string {
  return `${origin}/portal/#files`;
}

/** What the handoff needs from the page: opening a window and hearing messages. Tests stand in for it. */
export interface Host {
  open(url: string): Window | null;
  listen(handler: (event: MessageEvent) => void): () => void;
  setTimeout(handler: () => void, ms: number): number;
  clearTimeout(id: number): void;
  setInterval(handler: () => void, ms: number): number;
  clearInterval(id: number): void;
}

export const browserHost: Host = {
  open: (url) => window.open(url, "barracuda-portal"),
  listen: (handler) => {
    window.addEventListener("message", handler);
    return () => window.removeEventListener("message", handler);
  },
  setTimeout: (handler, ms) => window.setTimeout(handler, ms),
  clearTimeout: (id) => window.clearTimeout(id),
  setInterval: (handler, ms) => window.setInterval(handler, ms),
  clearInterval: (id) => window.clearInterval(id),
};

/** How long the portal has to say it is ready. */
export const READY_MS = 20_000;

export class Handoff {
  phase: Phase = "idle";
  failure: Failure | null = null;
  private win: Window | null = null;
  private origin = "";
  private offer: Offer | null = null;
  private stopListening: (() => void) | null = null;
  private timer = 0;
  private watcher = 0;

  constructor(
    private readonly host: Host,
    private readonly changed: () => void,
  ) {}

  private set(phase: Phase, failure: Failure | null = null) {
    this.phase = phase;
    this.failure = failure;
    // A failed write leaves the portal window open to try again, so it keeps listening.
    const over =
      phase === "closed" ||
      phase === "done" ||
      (phase === "failed" && failure?.kind !== "device");
    if (over) this.release(false);
    this.changed();
  }

  /** Opens the portal at `origin` and hands it `offer` once it says it is ready. */
  start(origin: string, offer: Offer) {
    this.release(false);
    this.offer = offer;
    this.origin = origin;
    this.win = this.host.open(portalUrl(origin));
    if (!this.win) return this.set("failed", { kind: "blocked" });
    this.stopListening = this.host.listen((event) => this.heard(event));
    this.timer = this.host.setTimeout(() => {
      if (this.phase === "opening") this.set("failed", { kind: "silent" });
    }, READY_MS);
    this.watcher = this.host.setInterval(() => {
      if (this.win?.closed && this.phase !== "done") this.set("closed");
    }, 500);
    this.set("opening");
  }

  /** Stops waiting and closes the portal window. */
  cancel() {
    this.release(true);
    this.set("idle");
  }

  private release(close: boolean) {
    this.host.clearTimeout(this.timer);
    this.host.clearInterval(this.watcher);
    this.stopListening?.();
    this.stopListening = null;
    if (close && this.win && !this.win.closed) this.win.close();
    if (close) this.win = null;
  }

  private heard(event: MessageEvent) {
    // Only the window this page opened, at the address the person gave, speaks for the device.
    if (!this.win || event.source !== this.win || event.origin !== this.origin)
      return;
    const data = event.data as {
      type?: unknown;
      v?: unknown;
      id?: unknown;
      state?: unknown;
      message?: unknown;
    } | null;
    if (!data || data.v !== PROTOCOL) return;
    if (data.type === "barracuda/ready" && this.offer) {
      const { id, name, version } = this.offer;
      const files: Record<string, ArrayBuffer> = {};
      for (const [file, bytes] of this.offer.files)
        files[file] = bytes.slice().buffer;
      this.win.postMessage(
        { type: "barracuda/app", v: PROTOCOL, id, name, version, files },
        this.origin,
        Object.values(files),
      );
      this.host.clearTimeout(this.timer);
      this.set("ready");
      return;
    }
    if (data.type !== "barracuda/status" || data.id !== this.offer?.id) return;
    switch (data.state) {
      case "review":
        return this.set("ready");
      case "writing":
        return this.set("writing");
      case "installed":
        return this.set("done");
      case "refused":
        return this.set("failed", { kind: "refused" });
      case "failed":
        return this.set("failed", {
          kind: "device",
          message:
            typeof data.message === "string"
              ? data.message
              : "The device could not write the app",
        });
    }
  }
}
