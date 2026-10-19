/**
 * An app from Live Editor. Live Editor runs on an HTTPS site, and a browser does not let such a
 * page call `http://` addresses on the local network, so it opens this page in a window and hands
 * it the app's files with `postMessage`. Nothing changes on the device until the person chooses
 * Install here; the page then writes `/workspace/media/apps/.<id>.bapp/` and swaps it in for
 * `<id>.bapp/`, which installs or updates the app (docs/plugins/app-manager/app-format.mdx).
 * docs/plugins/files.mdx defines the messages.
 */

import {
  button,
  callDevice,
  deviceError,
  h,
  icon,
  kv,
  pick,
  term,
  toastDeviceError,
  type DeviceResult,
  type Lang,
  type PortalContext,
  type Text,
} from "../../../captive-portal/resources/web/ui";

/** The version of the messages both sides speak. */
export const PROTOCOL = 1;
/** Where installable apps live. */
export const APPS = "/workspace/media/apps";
/** The most files and bytes an app may bring: its manifest, program, view and fonts. */
const MAX_FILES = 32;
const MAX_BYTES = 2 * 1024 * 1024;

const ICON_DOWNLOAD =
  '<path d="M12 17V3"></path><path d="m6 11 6 6 6-6"></path><path d="M19 21H5"></path>';
const ICON_CHECK = '<path d="M20 6 9 17l-5-5"></path>';
const ICON_SPINNER = '<path d="M21 12a9 9 0 1 1-6.219-8.56"></path>';

const t = {
  title: { zh: "Live Editor 发来一个应用", en: "Live Editor sent an app" },
  from: { zh: "来自", en: "From" },
  opener: { zh: "打开这个窗口的网页", en: "the page that opened this window" },
  openerTip: {
    zh: "只接受打开这个窗口的网页发来的应用",
    en: "Only the page that opened this window can send an app here",
  },
  app: { zh: "应用", en: "App" },
  version: { zh: "版本", en: "Version" },
  fresh: { zh: "这台设备上还没有", en: "new on this device" },
  replaces: { zh: "替换已安装的", en: "replaces" },
  files: { zh: "文件", en: "Files" },
  location: { zh: "位置", en: "Location" },
  hint: {
    zh: "安装后它会出现在设备的启动器里",
    en: "Once installed, it shows on the device's launcher",
  },
  refuse: { zh: "不安装", en: "Don't install" },
  install: { zh: "安装到设备", en: "Install on device" },
  writing: { zh: "正在写入工作区", en: "Writing to the Workspace" },
  done: {
    zh: "已安装。Live Editor 已收到结果，可以关闭这个窗口",
    en: "Installed. Live Editor knows; you can close this window",
  },
  refused: {
    zh: "没有安装，设备上什么都没变。可以关闭这个窗口",
    en: "Nothing was installed. You can close this window",
  },
  failed: { zh: "没有装上", en: "The app was not installed" },
} satisfies Record<string, Text>;

/** An app as Live Editor sends it. */
export interface Offer {
  id: string;
  name: string;
  version: string;
  files: { name: string; bytes: ArrayBuffer }[];
}

export type State = "review" | "writing" | "installed" | "refused";

/** The well-formed `barracuda/app` in `data`, or nothing. */
export function asOffer(data: unknown): Offer | undefined {
  if (!data || typeof data !== "object") return undefined;
  const { type, v, id, name, version, files } = data as Record<string, unknown>;
  if (type !== "barracuda/app" || v !== PROTOCOL) return undefined;
  if (typeof id !== "string" || !/^[a-z0-9-]{1,32}$/.test(id)) return undefined;
  if (typeof name !== "string" || typeof version !== "string") return undefined;
  if (!files || typeof files !== "object" || Array.isArray(files))
    return undefined;
  const list = Object.entries(files as Record<string, unknown>);
  if (list.length === 0 || list.length > MAX_FILES) return undefined;
  let total = 0;
  for (const [file, bytes] of list) {
    if (!/^[A-Za-z0-9_.-]{1,64}$/.test(file) || file === "." || file === "..")
      return undefined;
    if (!(bytes instanceof ArrayBuffer)) return undefined;
    total += bytes.byteLength;
  }
  if (total > MAX_BYTES || !list.some(([file]) => file === "app.json"))
    return undefined;
  return {
    id,
    name: name.slice(0, 64),
    version: version.slice(0, 32),
    files: list.map(([file, bytes]) => ({
      name: file,
      bytes: bytes as ArrayBuffer,
    })),
  };
}

const size = (bytes: number) =>
  bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KB`;

/** What the device answers, for a call that may fail. */
type Call = DeviceResult<unknown>;

/**
 * Takes an app from the window that opened this page and writes it where the app manager finds
 * it, once the person chooses to. Renders its card into `slot`; `installed` runs once the app is
 * in place, so the page can show it.
 */
export class Incoming {
  private offer: Offer | null = null;
  private origin = "";
  private state: State = "review";
  private progress = 0;
  /** The installed version this app replaces, once known. */
  private replaces: string | null = null;
  private asked = false;

  /**
   * `asking` hears whether the card holds the page's primary action, Install, so the page can
   * quiet its own.
   */
  constructor(
    private readonly context: PortalContext,
    private readonly slot: HTMLElement,
    private readonly installed: (path: string) => void,
    private readonly opener: Window | null = window.opener as Window | null,
    private readonly asking: (asking: boolean) => void = () => {},
  ) {}

  /** Tells the opener this page is ready, and listens to it. */
  start() {
    if (!this.opener) return;
    window.addEventListener("message", (event) => this.heard(event), {
      signal: this.context.signal,
    });
    this.opener.postMessage({ type: "barracuda/ready", v: PROTOCOL }, "*");
  }

  private heard(event: MessageEvent) {
    // Only the window that opened this one, and not while it is writing.
    if (event.source !== this.opener || this.state === "writing") return;
    const offer = asOffer(event.data);
    if (!offer) return;
    this.offer = offer;
    this.origin = event.origin;
    this.replaces = null;
    this.settle("review");
    void this.look(offer);
  }

  /** Finds the version this app would replace, from its installed manifest. */
  private async look(offer: Offer) {
    const result = await callDevice<{ version?: string }>(
      this.context,
      `/api/files/raw/workspace/media/apps/${encodeURIComponent(`${offer.id}.bapp`)}/app.json`,
    );
    if (this.offer !== offer) return;
    if (result.kind === "ok") {
      this.replaces =
        typeof result.data?.version === "string" ? result.data.version : "";
      this.render();
    }
  }

  private report(state: State | "failed", message?: string) {
    if (!this.offer || !this.opener) return;
    this.opener.postMessage(
      {
        type: "barracuda/status",
        v: PROTOCOL,
        id: this.offer.id,
        state,
        message,
      },
      this.origin,
    );
  }

  private settle(state: State) {
    this.state = state;
    this.report(state);
    this.render();
  }

  private change(
    route: "mkdir" | "rename" | "delete",
    body: unknown,
  ): Promise<Call> {
    return callDevice(this.context, `/api/files/${route}`, {
      method: "POST",
      body,
    });
  }

  private async list(path: string) {
    return callDevice<{ entries: { name: string; type: string }[] }>(
      this.context,
      `/api/files/list/${path.split("/").filter(Boolean).map(encodeURIComponent).join("/")}`,
    );
  }

  /** Deletes an app directory and its files; a missing one is already gone. */
  private async remove(path: string): Promise<Call> {
    const listing = await this.list(path);
    if (listing.kind === "error" && listing.error.status === 404)
      return { kind: "ok", status: 204, data: null };
    if (listing.kind !== "ok") return listing;
    for (const entry of listing.data?.entries ?? []) {
      const gone = await this.change("delete", {
        path: `${path}/${entry.name}`,
      });
      if (gone.kind !== "ok") return gone;
    }
    return this.change("delete", { path });
  }

  private async upload(path: string, bytes: ArrayBuffer): Promise<Call> {
    try {
      const response = await fetch(
        `/api/files/upload/${path.split("/").filter(Boolean).map(encodeURIComponent).join("/")}`,
        {
          method: "PUT",
          body: bytes,
          signal: this.context.signal,
          cache: "no-store",
          redirect: "error",
        },
      );
      return response.ok
        ? { kind: "ok", status: response.status, data: null }
        : { kind: "error", error: await deviceError(response) };
    } catch {
      return this.context.signal.aborted
        ? { kind: "aborted" }
        : { kind: "offline" };
    }
  }

  /** Writes the app beside its place, then swaps it in, so the app manager never sees half of it. */
  private async install() {
    const offer = this.offer;
    if (!offer || this.state !== "review") return;
    this.progress = 0;
    this.settle("writing");
    const stage = `${APPS}/.${offer.id}.bapp`;
    const place = `${APPS}/${offer.id}.bapp`;
    const old = `${APPS}/.${offer.id}.bapp.old`;
    const steps: (() => Promise<Call>)[] = [
      async () => {
        const made = await this.change("mkdir", { path: APPS });
        return made.kind === "error" && made.error.status === 409
          ? { kind: "ok", status: 200, data: null }
          : made;
      },
      () => this.remove(stage),
      () => this.remove(old),
      () => this.change("mkdir", { path: stage }),
      ...offer.files.map(
        (file) => () => this.upload(`${stage}/${file.name}`, file.bytes),
      ),
      async () => {
        const moved = await this.change("rename", { from: place, to: old });
        return moved.kind === "error" && moved.error.status === 404
          ? { kind: "ok", status: 200, data: null }
          : moved;
      },
      () => this.change("rename", { from: stage, to: place }),
      () => this.remove(old),
    ];
    for (const [index, step] of steps.entries()) {
      const result = await step();
      if (result.kind === "aborted") return;
      if (result.kind !== "ok") {
        this.state = "review";
        this.report(
          "failed",
          result.kind === "error"
            ? (result.error.message ?? `HTTP ${result.error.status}`)
            : undefined,
        );
        toastDeviceError(this.context, result);
        this.render();
        return;
      }
      this.progress = Math.round(((index + 1) / steps.length) * 100);
      this.render();
    }
    this.settle("installed");
    this.installed(place);
  }

  private render() {
    const offer = this.offer;
    const asking = !!offer && this.state === "review";
    if (asking !== this.asked) this.asking((this.asked = asking));
    // An empty slot would still take its place in the page's gaps.
    this.slot.hidden = !offer;
    if (!offer) return this.slot.replaceChildren();
    const lang: Lang = this.context.lang;
    const total = offer.files.reduce(
      (sum, file) => sum + file.bytes.byteLength,
      0,
    );
    const mono = (text: string) => h("span", { class: "bc-mono" }, text);
    const status = (
      className: string,
      mark: string | null,
      text: Text,
      spin = false,
    ) =>
      h(
        "span",
        { class: `bc-status ${className}`, role: "status" },
        mark ? icon(mark, undefined, spin ? "bc-spinner" : undefined) : null,
        pick(text, lang),
      );
    const foot = h("div", { class: "bc-card__foot" });
    foot.style.justifyContent = "flex-end";
    const lead = (node: HTMLElement) => {
      node.style.cssText = "flex:1 1 240px;margin-right:auto";
      return node;
    };
    if (this.state === "review")
      foot.append(
        lead(h("span", { class: "bc-hint" }, pick(t.hint, lang))),
        button(t.refuse, lang, {
          variant: "outline",
          size: "sm",
          onClick: () => this.settle("refused"),
        }),
        button(t.install, lang, {
          size: "sm",
          onClick: () => void this.install(),
        }),
      );
    else if (this.state === "writing")
      foot.append(
        lead(status("bc-muted", ICON_SPINNER, t.writing, true)),
        h(
          "span",
          { class: "bc-mono bc-caption bc-muted" },
          `${this.progress}%`,
        ),
      );
    else if (this.state === "installed")
      foot.append(lead(status("bc-success", ICON_CHECK, t.done)));
    else foot.append(lead(status("bc-muted", null, t.refused)));
    const card = h(
      "section",
      {
        class: `bc-card${this.state === "writing" ? " bc-card--progress" : ""}`,
        "aria-labelledby": "files-incoming",
      },
      h(
        "div",
        { class: "bc-card__head" },
        icon(ICON_DOWNLOAD, undefined, "bc-muted"),
        h(
          "h2",
          { id: "files-incoming", class: "bc-title" },
          pick(t.title, lang),
        ),
      ),
      h(
        "div",
        { class: "bc-card__body" },
        kv(
          [
            [
              t.from,
              [
                mono(this.origin),
                " · ",
                term(t.opener, t.openerTip, lang, { start: true }),
              ],
            ],
            [t.app, [offer.name, " · ", mono(offer.id)]],
            [
              t.version,
              [
                mono(offer.version),
                " · ",
                this.replaces === null
                  ? pick(t.fresh, lang)
                  : [pick(t.replaces, lang), " ", mono(this.replaces || "?")],
              ],
            ],
            [
              t.files,
              [mono(String(offer.files.length)), " · ", mono(size(total))],
            ],
            [t.location, mono(`media/apps/${offer.id}.bapp`)],
          ],
          lang,
        ),
      ),
      foot,
    );
    if (this.state === "writing")
      card.style.setProperty("--progress", `${this.progress}%`);
    this.slot.replaceChildren(card);
  }
}
