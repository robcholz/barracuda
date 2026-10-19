import {
  button,
  callDevice,
  definePage,
  deviceError,
  entryStatus,
  h,
  header,
  kv,
  note,
  page,
  pick,
  term,
  toastDeviceError,
  type Lang,
  type PortalContext,
  type Text,
} from "../../../captive-portal/resources/web/ui";

/** An offer in the Workspace; `incompatible` says why this device cannot install it. */
interface Offer {
  id: string;
  name: string;
  version: string;
  incompatible?: string;
}

/** The install waiting for the person: what it asks for, and the digest the install is pinned to. */
interface Waiting {
  id: string;
  name: string;
  version: string;
  digest: string | null;
  signer: string | null;
  asks: string[];
}

/** `GET /api/apps`. */
interface Listing {
  waiting: Waiting | null;
  offers: Offer[];
  installed: { id: string; version: string }[];
  /** Whether this browser is on the setup hotspot, where it may install. */
  hotspot: boolean;
}

/**
 * A package Live Editor sent: it opens this page in a window, waits for `barracuda/ready`, then
 * posts `barracuda/offer` with the `.bapp` bytes. Nothing reaches the device until the person
 * chooses to send it here, and the install still waits for someone at the device.
 */
interface Incoming {
  origin: string;
  id: string;
  name: string;
  version: string;
  bytes: ArrayBuffer;
  state:
    "review" | "sending" | "waiting" | "installed" | "declined" | "refused";
}

/** The protocol version both sides speak; docs/plugins/app-portal.mdx defines it. */
const PROTOCOL = 1;
/** As `MAX_PACKAGE_BYTES` in the app manager. */
const MAX_PACKAGE_BYTES = 1024 * 1024;
const POLL_MS = 2000;

const t = {
  lead: {
    zh: "安装工作区里提供的应用。安装由设备旁的人确认：在设备屏幕上，或用连上设备热点的手机。",
    en: "Install the apps the Workspace offers. The person at the device confirms each install: on its screen, or from a phone on its setup hotspot.",
  },
  status: { zh: "状态", en: "Status" },
  confirm: { zh: "确认安装", en: "Confirm" },
  here: { zh: "可以在这里确认", en: "Here" },
  there: {
    zh: "在设备屏幕上，或设备热点上的手机",
    en: "On the device screen, or a phone on its setup hotspot",
  },
  sentTitle: { zh: "Live Editor 发来一个应用", en: "Live Editor sent an app" },
  from: { zh: "来自", en: "From" },
  opener: { zh: "打开这个窗口的网页", en: "the page that opened this window" },
  openerTip: {
    zh: "只接受打开这个窗口的网页发来的应用",
    en: "Only the page that opened this window can send an app here",
  },
  app: { zh: "应用", en: "App" },
  version: { zh: "版本", en: "Version" },
  size: { zh: "大小", en: "Size" },
  signer: { zh: "签名", en: "Signer" },
  unsigned: { zh: "未签名", en: "Unsigned" },
  reviewHint: {
    zh: "发送后，安装仍要在设备上确认",
    en: "The install still needs a yes on the device",
  },
  refuse: { zh: "不发送", en: "Don't send" },
  send: { zh: "发送到设备", en: "Send to device" },
  sending: { zh: "正在保存到工作区…", en: "Saving to the Workspace…" },
  sent: {
    zh: "已发送，等待设备上确认",
    en: "Sent. Waiting for a yes on the device",
  },
  done: {
    zh: "已安装。Live Editor 已收到结果，可以关闭这个窗口",
    en: "Installed. Live Editor knows; you can close this window",
  },
  declined: {
    zh: "设备上拒绝了安装。可以关闭这个窗口",
    en: "Declined on the device. You can close this window",
  },
  refused: {
    zh: "没有发送，设备上什么都没变。可以关闭这个窗口",
    en: "Nothing was sent. You can close this window",
  },
  waitingTitle: { zh: "等待确认", en: "Waiting" },
  none: { zh: "没有等待确认的安装。", en: "No install is waiting." },
  asks: { zh: "它会", en: "It will" },
  noAsks: { zh: "不需要额外权限", en: "No extra permissions" },
  install: { zh: "安装", en: "Install" },
  decline: { zh: "拒绝", en: "Decline" },
  awayA: {
    zh: "在设备屏幕上点「安装」，或用",
    en: "Tap Install on the device screen, or confirm from",
  },
  awayTerm: { zh: "连上设备热点的手机", en: "a phone on the setup hotspot" },
  awayTip: {
    zh: "设备的配置热点开着时，热点上的手机或电脑可以在这一页确认",
    en: "While the setup hotspot is on, a phone or computer on it can confirm on this page",
  },
  awayB: { zh: "打开这一页确认。", en: "." },
  workspace: { zh: "工作区", en: "Workspace" },
  request: { zh: "请求安装", en: "Ask to install" },
  noOffers: {
    zh: "工作区里没有可安装的应用。",
    en: "The Workspace offers no apps.",
  },
} satisfies Record<string, Text>;

/** Whether `data` is a well-formed `barracuda/offer`. */
function asOffer(
  data: unknown,
): Omit<Incoming, "origin" | "state"> | undefined {
  if (!data || typeof data !== "object") return undefined;
  const { type, v, id, name, version, bapp } = data as Record<string, unknown>;
  if (type !== "barracuda/offer" || v !== PROTOCOL) return undefined;
  if (typeof id !== "string" || !/^[a-z0-9-]{1,32}$/.test(id)) return undefined;
  if (typeof name !== "string" || typeof version !== "string") return undefined;
  if (!(bapp instanceof ArrayBuffer)) return undefined;
  if (bapp.byteLength === 0 || bapp.byteLength > MAX_PACKAGE_BYTES)
    return undefined;
  return {
    id,
    name: name.slice(0, 64),
    version: version.slice(0, 32),
    bytes: bapp,
  };
}

function kib(bytes: number): string {
  return `${(bytes / 1024).toFixed(1)} KB`;
}

function heading(text: Text, lang: Lang, count?: number): HTMLElement {
  const node = h(
    "h2",
    { class: "bc-title" },
    pick(text, lang),
    count === undefined
      ? null
      : h(
          "span",
          { class: "bc-mono bc-caption bc-muted" },
          String(count).padStart(2, "0"),
        ),
  );
  node.style.cssText = "display:flex;align-items:baseline;gap:8px;margin:0";
  return node;
}

function card(...parts: (HTMLElement | null)[]): HTMLElement {
  return h("section", { class: "bc-card" }, ...parts);
}

function foot(...children: (Node | null)[]): HTMLElement {
  const node = h("div", { class: "bc-card__foot" }, ...children);
  node.style.justifyContent = "flex-end";
  return node;
}

/** A line at the left of a card's foot: a hint or a status. */
function lead(node: HTMLElement): HTMLElement {
  node.style.cssText = "flex:1 1 240px;margin-right:auto";
  return node;
}

/**
 * The apps page: the install waiting for the person, with what it asks for and who signed it, and
 * the Workspace's offers. Installing or declining works only from the device's setup hotspot, so
 * only someone at the device decides; asking for an install works from anywhere. Opened by Live
 * Editor, the page also takes the package it sends.
 */
export const mount = definePage((context: PortalContext) => {
  const lang = context.lang;
  const status = entryStatus(context);
  const confirmSlot = h("span", null, "—");
  const incomingSlot = h("div");
  const waiting = h("div");
  const offers = h("div");
  const offersHeading = h("div", null, heading(t.workspace, lang));
  let incoming: Incoming | null = null;
  let poll: ReturnType<typeof setTimeout> | undefined;
  context.signal.addEventListener("abort", () => clearTimeout(poll));

  /** Tells the page that opened this window how its package is doing. */
  const report = (state: Incoming["state"] | "failed", message?: string) => {
    if (!incoming || !window.opener) return;
    (window.opener as Window).postMessage(
      {
        type: "barracuda/status",
        v: PROTOCOL,
        id: incoming.id,
        state,
        message,
      },
      incoming.origin,
    );
  };

  const settle = (state: Incoming["state"]) => {
    if (!incoming) return;
    incoming.state = state;
    report(state);
    drawIncoming();
  };

  const call = async (
    endpoint: string,
    body: unknown,
    after: () => Promise<void>,
  ) => {
    const result = await callDevice(context, endpoint, {
      method: "POST",
      body,
    });
    if (result.kind === "ok") {
      await after();
    } else if (result.kind !== "aborted") {
      toastDeviceError(context, result);
    }
  };

  const send = async () => {
    if (!incoming || incoming.state !== "review") return;
    const sent = incoming;
    settle("sending");
    let response: Response;
    try {
      response = await fetch(`/api/apps/offer/${sent.id}`, {
        method: "PUT",
        headers: { "Content-Type": "application/octet-stream" },
        body: sent.bytes,
        signal: context.signal,
        cache: "no-store",
      });
    } catch {
      if (context.signal.aborted) return;
      sent.state = "review";
      report("failed");
      drawIncoming();
      toastDeviceError(context, { kind: "offline" }, () => void send());
      return;
    }
    if (!response.ok) {
      const error = await deviceError(response);
      sent.state = "review";
      report("failed", error.message);
      drawIncoming();
      toastDeviceError(context, { kind: "error", error });
      return;
    }
    const app = (await response.json()) as Waiting;
    sent.version = app.version;
    settle("waiting");
    await load();
  };

  const drawIncoming = () => {
    incomingSlot.hidden = !incoming;
    if (!incoming) return;
    const it = incoming;
    const body = kv(
      [
        [
          t.from,
          [
            h("span", { class: "bc-mono" }, it.origin),
            " · ",
            term(t.opener, t.openerTip, lang, { start: true }),
          ],
        ],
        [t.app, [it.name, " · ", h("span", { class: "bc-mono" }, it.id)]],
        [t.version, h("span", { class: "bc-mono" }, it.version)],
        [t.size, h("span", { class: "bc-mono" }, kib(it.bytes.byteLength))],
      ],
      lang,
    );
    body.classList.add("bc-card__body", "bc-kv--dense");
    body.style.margin = "0";
    const line = (text: Text, tone: string) =>
      lead(
        h(
          "span",
          { class: `bc-status ${tone}`, role: "status" },
          pick(text, lang),
        ),
      );
    const ending = {
      review: [
        lead(h("span", { class: "bc-hint" }, pick(t.reviewHint, lang))),
        button(t.refuse, lang, {
          variant: "outline",
          size: "sm",
          onClick: () => settle("refused"),
        }),
        button(t.send, lang, { size: "sm", onClick: () => void send() }),
      ],
      sending: [line(t.sending, "bc-muted")],
      waiting: [line(t.sent, "bc-muted")],
      installed: [line(t.done, "bc-success")],
      declined: [line(t.declined, "bc-muted")],
      refused: [line(t.refused, "bc-muted")],
    }[it.state];
    const head = h(
      "div",
      { class: "bc-card__head" },
      h("h2", { class: "bc-title" }, pick(t.sentTitle, lang)),
    );
    incomingSlot.replaceChildren(card(head, body, foot(...ending)));
  };

  const drawWaiting = (list: Listing) => {
    const app = list.waiting;
    if (!app) {
      const empty = h("p", { class: "bc-list-note" }, pick(t.none, lang));
      empty.style.borderTop = "0";
      waiting.replaceChildren(h("section", { class: "bc-frame" }, empty));
      return;
    }
    const signed = app.signer
      ? [
          `${pick(t.signer, lang)} `,
          h("span", { class: "bc-mono" }, app.signer),
        ]
      : pick(t.unsigned, lang);
    const asks = app.asks.length ? app.asks : [pick(t.noAsks, lang)];
    const decide = list.hotspot
      ? [
          button(t.decline, lang, {
            variant: "outline",
            size: "sm",
            onClick: () => void call("/api/apps/decline", { id: app.id }, load),
          }),
          button(t.install, lang, {
            size: "sm",
            onClick: () =>
              void call(
                "/api/apps/install",
                { id: app.id, digest: app.digest },
                load,
              ),
          }),
        ]
      : [
          lead(
            h(
              "span",
              { class: "bc-hint" },
              `${pick(t.awayA, lang)} `,
              term(t.awayTerm, t.awayTip, lang, { start: true }),
              pick(t.awayB, lang),
            ),
          ),
        ];
    const title = h(
      "div",
      { class: "bc-card__head" },
      h(
        "div",
        null,
        h(
          "span",
          { class: "bc-option-title" },
          lang === "zh" ? `安装「${app.name}」？` : `Install “${app.name}”?`,
        ),
        h(
          "span",
          { class: "bc-small bc-muted" },
          `${pick(t.version, lang)} `,
          h("span", { class: "bc-mono" }, app.version),
          " · ",
          signed,
        ),
      ),
    );
    (title.firstElementChild as HTMLElement).style.cssText =
      "display:flex;flex-direction:column;gap:2px;min-width:0";
    const asked = h(
      "div",
      { class: "bc-card__body" },
      h("span", { class: "bc-small bc-muted" }, pick(t.asks, lang)),
      h(
        "ul",
        { class: "bc-small" },
        asks.map((line) => h("li", null, line)),
      ),
    );
    waiting.replaceChildren(card(title, asked, foot(...decide)));
  };

  const drawOffers = (list: Listing) => {
    const rows = list.offers.map((offer) => {
      const name = h(
        "div",
        null,
        h(
          "span",
          { class: "bc-option-title" },
          `${offer.name} `,
          h("span", { class: "bc-mono bc-small bc-muted" }, offer.id),
        ),
        h(
          "span",
          { class: "bc-small bc-muted" },
          offer.incompatible ?? `${pick(t.version, lang)} ${offer.version}`,
        ),
      );
      name.style.cssText =
        "flex:1 1 200px;min-width:0;display:flex;flex-direction:column;gap:2px";
      const asking = list.waiting?.id === offer.id;
      const action =
        offer.incompatible || asking
          ? asking
            ? h("span", { class: "bc-badge" }, pick(t.waitingTitle, lang))
            : null
          : button(t.request, lang, {
              variant: "outline",
              size: "sm",
              onClick: () =>
                void call("/api/apps/request", { id: offer.id }, load),
            });
      const row = h("div", { class: "bc-card__row" }, name, action);
      row.style.flexWrap = "wrap";
      return row;
    });
    offersHeading.replaceChildren(heading(t.workspace, lang, rows.length));
    offers.replaceChildren(
      rows.length
        ? card(...rows)
        : h("section", { class: "bc-frame" }, note(t.noOffers, lang)),
    );
  };

  /** Follows the package Live Editor sent until the person at the device decides. */
  const follow = (list: Listing) => {
    clearTimeout(poll);
    if (!incoming || incoming.state !== "waiting") return;
    const it = incoming;
    if (list.waiting?.id === it.id) {
      poll = setTimeout(() => void load(), POLL_MS);
    } else if (
      list.installed.some(
        (app) => app.id === it.id && app.version === it.version,
      )
    ) {
      settle("installed");
    } else {
      settle("declined");
    }
  };

  const load = async () => {
    const result = await callDevice<Listing>(context, "/api/apps");
    if (result.kind !== "ok" || !result.data) {
      if (result.kind === "error" || result.kind === "offline")
        toastDeviceError(context, result, () => void load());
      return;
    }
    const listing = result.data;
    void status.refresh();
    confirmSlot.replaceChildren(pick(listing.hotspot ? t.here : t.there, lang));
    drawWaiting(listing);
    drawOffers(listing);
    follow(listing);
  };

  // Opened by Live Editor: say the page is ready, then take one package from that window only.
  if (window.opener) {
    window.addEventListener(
      "message",
      (event: MessageEvent) => {
        if (event.source !== window.opener || incoming?.state === "sending")
          return;
        const offer = asOffer(event.data);
        if (!offer) return;
        incoming = { ...offer, origin: event.origin, state: "review" };
        report("review");
        drawIncoming();
      },
      { signal: context.signal },
    );
    (window.opener as Window).postMessage(
      { type: "barracuda/ready", v: PROTOCOL },
      "*",
    );
  }
  void load();

  incomingSlot.hidden = true;
  const group = (...children: HTMLElement[]) => {
    const node = h("div", null, ...children);
    node.style.cssText = "display:flex;flex-direction:column;gap:12px";
    return node;
  };
  return page(
    header(
      {
        title: { zh: "应用", en: "Apps" },
        lead: t.lead,
        extra: kv(
          [
            [t.status, status.slot],
            [t.confirm, confirmSlot],
          ],
          lang,
          { live: true },
        ),
        figure: "board",
      },
      lang,
    ),
    incomingSlot,
    group(heading(t.waitingTitle, lang), waiting),
    group(offersHeading, offers),
  );
});
