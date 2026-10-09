import type { Lang, PortalContext } from "../src/contract";
import type { ChannelState } from "./channel";
import { callDevice, toastDeviceError } from "./device";
import { h, icon, pick, type Text } from "./dom";
import { fitColumns } from "./grid";
import { ICON_CIRCLE_ALERT, ICON_REFRESH } from "./icons";
import { badge, button, row, term } from "./layout";
import type { SettingsForm } from "./settings";

/**
 * The inbound side of an external channel (the design's `mode` and `owners` blocks): how the device
 * uses the channel, its live receive state, and the accounts allowed to command the device with the
 * binding code that adds one. Every channel serves the same JSON under its config path.
 */

/** `disabled` pauses the channel, `send` only sends, `send_receive` also holds a receive connection. */
export type ChannelMode = "disabled" | "send" | "send_receive";
/** Every mode, in the order the control shows them. WeChat offers all but `send`. */
export const CHANNEL_MODES: readonly ChannelMode[] = [
  "disabled",
  "send",
  "send_receive",
];
export type ReceiveState =
  "idle" | "starting" | "receiving" | "no_slot" | "error";

/**
 * `GET <channel endpoint>`: `receive` only in `send_receive`, `message` only with `error`, `capacity`
 * only with `no_slot`, `slots` (the device's receive slots held now, across every channel) whenever
 * the channel draws from that bounded pool (a webhook channel holds none).
 */
export interface ChannelStatus extends ChannelState {
  mode?: ChannelMode;
  receive?: {
    state: ReceiveState;
    message?: string;
    capacity?: number;
    slots?: { in_use: number; capacity: number };
  };
  owners?: { count: number };
}

/** An allowed account: the channel's own sender ID and the name it gave, if any. */
export interface Owner {
  id: string;
  label: string | null;
}

/** `GET <channel endpoint>/owners`: `pairing` is `null` while the list is full or no code can be minted. */
export interface OwnersReply {
  owners: Owner[];
  pairing: { code: string; expires_in: number } | null;
  ignored: number;
}

const STRINGS = {
  zh: {
    mode: "模式",
    modeHint: "选择设备怎样使用这个通道",
    modes: {
      disabled: ["停用", "保留凭据，暂停通道"],
      send: ["仅发送", "Agent 主动发消息"],
      send_receive: ["收发", "接收消息并回复"],
    },
    receiving: "收发中",
    connecting: "连接中",
    waiting: "等待名额",
    broken: "连接中断",
    sendOnly: "仅发送",
    disabled: "已停用",
    slots: (used: number, n: number) => `名额 ${used}/${n}`,
    slotsTip: "每个收发通道保持一条连接，名额由设备内存决定",
    full: (n: number) => `收发通道已达上限（${n}）`,
    // a Latin name keeps a space before the Chinese that follows it (「QQ 会」, 「微信会」)
    fullBody: (channel: string) =>
      `把其他通道改为仅发送或停用后，${channel}${/[\x21-\x7e]$/.test(channel) ? " " : ""}会自动开始接收。`,
    accounts: "授权账号",
    accountsHint: "能给设备下指令的账号",
    expiresIn: "有效期还剩",
    rotate: "换一个绑定码",
    remove: "移除",
    empty: "还没有授权账号",
  },
  en: {
    mode: "Mode",
    modeHint: "Choose how the device uses this channel",
    modes: {
      disabled: ["Disabled", "Keeps the credentials and pauses the channel"],
      send: ["Send only", "The agent sends messages"],
      send_receive: ["Send and receive", "Receives messages and replies"],
    },
    receiving: "Receiving",
    connecting: "Connecting",
    waiting: "Waiting for a slot",
    broken: "Disconnected",
    sendOnly: "Send only",
    disabled: "Disabled",
    slots: (used: number, n: number) => `Slots ${used} of ${n}`,
    slotsTip:
      "Each receiving channel keeps one connection open; the device memory sets how many",
    full: (n: number) => `Receive slots are full (${n})`,
    fullBody: (channel: string) =>
      `Set another channel to Send only or Disabled and ${channel} starts receiving on its own.`,
    accounts: "Allowed accounts",
    accountsHint: "Accounts that can command the device",
    expiresIn: "Expires in",
    rotate: "New code",
    remove: "Remove",
    empty: "No allowed accounts yet",
  },
} satisfies Record<Lang, unknown>;

const isMode = (value: unknown): value is ChannelMode =>
  CHANNEL_MODES.includes(value as ChannelMode);

const show = (node: HTMLElement, visible: boolean) =>
  (node.style.display = visible ? "" : "none");

export interface ModeRowOptions {
  /** The channel's config path; the mode goes to `POST <endpoint>/mode`. */
  endpoint: string;
  /** The channel's name in the no-slot alert (「QQ」, 「微信」). */
  channel: Text;
  /** The modes the channel offers (default {@link CHANNEL_MODES}). */
  modes?: readonly ChannelMode[];
  /** Runs after the device took a mode (204, or 409 `no_slot`, which still saves it). */
  onChange?: (mode: ChannelMode) => void;
}

export interface ModeRow {
  /** The 「模式」 row; hidden until {@link ModeRow.update} gets a configured channel with a mode. */
  element: HTMLElement;
  /** Shows the channel's mode and receive state, or hides the row (`null`, not configured). */
  update(status: ChannelStatus | null): void;
}

/**
 * The 「模式」 row: one radio card per mode, the receive state badge under them (收发中, 连接中,
 * 等待名额, 连接中断 with the device's message in mono) followed by the slots in use (「名额 1/2」)
 * whenever the device reports them and, while every receive slot is taken, an alert naming the limit. Choosing a mode posts `{mode}` at once; a refusal is
 * toasted and the choice goes back.
 */
export function modeRow(
  options: ModeRowOptions,
  context: PortalContext,
): ModeRow {
  const { lang } = context;
  const s = STRINGS[lang];
  const modes = options.modes ?? CHANNEL_MODES;
  const inputs = modes.map((mode) =>
    h("input", { type: "radio", name: "channel_mode", value: mode }),
  );
  const cards = modes.map((mode, index) => {
    const [label, hint] = s.modes[mode];
    const text = h(
      "span",
      null,
      h("span", { class: "bc-option-title" }, label),
      h("span", { class: "bc-hint" }, hint),
    );
    text.style.cssText = "flex:1 1 auto;min-width:0";
    return h("label", { class: "bc-radio-card" }, inputs[index], text);
  });
  const group = h("div", { role: "radiogroup", "aria-label": s.mode }, cards);
  group.style.cssText = `display:grid;grid-template-columns:${fitColumns(modes.length, 170, "var(--space-2)")};gap:var(--space-2)`;
  const state = h("span", { class: "bc-small bc-muted", role: "status" });
  state.style.cssText =
    "display:flex;flex-wrap:wrap;align-items:center;gap:8px;min-width:0";
  // shown while every receive slot is taken: a standing fact, so an alert rather than a toast
  const alertTitle = h("span", { class: "bc-alert__title" });
  const alertBody = h("span", { class: "bc-small bc-muted" });
  const alertText = h("span", null, alertTitle, alertBody);
  alertText.style.cssText =
    "display:flex;flex-direction:column;gap:2px;min-width:0";
  const alertMark = icon(ICON_CIRCLE_ALERT);
  alertMark.style.cssText = "flex:none;margin-top:2px";
  const alert = h(
    "div",
    { class: "bc-alert", role: "status" },
    alertMark,
    alertText,
  );
  const body = h("div", null, group, state);
  body.style.cssText = "display:flex;flex-direction:column;gap:10px";
  const element = row(s.mode, s.modeHint, lang, body, alert);
  show(element, false);

  /** The mode the device holds, and the one being posted. */
  let current: ChannelMode | null = null;
  let pending: ChannelMode | null = null;

  const check = (mode: ChannelMode | null) => {
    for (const input of inputs) {
      input.checked = input.value === mode;
      // 「清空」 resets the form: keep the device's mode
      input.defaultChecked = input.value === current;
    }
  };

  async function choose(mode: ChannelMode) {
    if (pending || mode === current) return check(pending ?? current);
    pending = mode;
    check(mode);
    const result = await callDevice(context, `${options.endpoint}/mode`, {
      method: "POST",
      body: { mode },
    });
    pending = null;
    if (result.kind === "aborted") return;
    const saved =
      result.kind === "ok" ||
      (result.kind === "error" && result.error.error === "no_slot");
    if (saved) {
      current = mode;
      check(mode);
      options.onChange?.(mode);
      return;
    }
    check(current);
    toastDeviceError(context, result, () => void choose(mode));
  }
  inputs.forEach((input, index) =>
    input.addEventListener("change", () => void choose(modes[index])),
  );

  const update = (status: ChannelStatus | null) => {
    if (!status?.configured || !isMode(status.mode)) {
      show(element, false);
      return;
    }
    show(element, true);
    current = status.mode;
    check(pending ?? current);
    const receive = status.receive;
    let label: string = s.connecting;
    let signal = false;
    let extra: Node | null = null;
    let full: number | null = null;
    // a device that predates `slots` reports only the limit, and only with no_slot
    let slots =
      status.mode === "send_receive" && receive?.slots ? receive.slots : null;
    if (status.mode === "disabled") label = s.disabled;
    else if (status.mode === "send") label = s.sendOnly;
    else if (receive?.state === "receiving") {
      label = s.receiving;
      signal = true;
    } else if (receive?.state === "no_slot") {
      label = s.waiting;
      const capacity = receive.capacity ?? receive.slots?.capacity;
      if (typeof capacity === "number") {
        full = capacity;
        slots ??= { in_use: capacity, capacity };
      }
    } else if (receive?.state === "error") {
      label = s.broken;
      if (receive.message) {
        extra = h("span", { class: "bc-mono" }, receive.message);
        (extra as HTMLElement).style.overflowWrap = "anywhere";
      }
    }
    let count: HTMLElement | null = null;
    if (slots) {
      count = term(s.slots(slots.in_use, slots.capacity), s.slotsTip, lang);
      count.classList.add("bc-mono");
    }
    state.replaceChildren(
      badge(label, lang, { signal }),
      count ?? "",
      extra ?? "",
    );
    if (full !== null) {
      alertTitle.textContent = s.full(full);
      alertBody.textContent = s.fullBody(pick(options.channel, lang));
    }
    show(alert, full !== null);
  };
  show(alert, false);
  return { element, update };
}

export interface AccountsRowOptions {
  /** The channel's config path; the list is `GET`/`POST <endpoint>/owners`. */
  endpoint: string;
  /** Where to send the code from, above it (「在 Telegram 里给 Bot 发送」). */
  how: Text;
  /** What to send for a code (Telegram: `/start <code>`); the bare code by default. */
  command?: (code: string) => string;
}

export interface AccountsRow {
  /** The 「授权账号」 row; hidden until the first list arrives. */
  element: HTMLElement;
  /** Reads the list and the binding code again and redraws; the row shows once one arrived. */
  load(): Promise<void>;
  hide(): void;
  /** Accounts in the list last read, or `null` before one arrived. */
  count(): number | null;
}

/**
 * The 「授权账号」 row: the binding code (「绑定码」) in large mono with where to send it and its
 * countdown, 「换一个绑定码」, then each allowed account with 「移除」. The countdown runs locally and
 * reads a fresh code when it ends; `pairing: null` (a full list) drops the code box. Removing asks
 * nothing: the device is the confirmation.
 */
export function accountsRow(
  options: AccountsRowOptions,
  context: PortalContext,
): AccountsRow {
  const { lang } = context;
  const s = STRINGS[lang];
  const card = h("div", { class: "bc-frame" });
  card.style.overflow = "hidden";
  const element = row(s.accounts, s.accountsHint, lang, card);
  show(element, false);
  const endpoint = `${options.endpoint}/owners`;
  let count: number | null = null;
  let tick: ReturnType<typeof setInterval> | undefined;
  let seq = 0;
  context.signal.addEventListener("abort", () => clearInterval(tick), {
    once: true,
  });

  const clock = (seconds: number) =>
    `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;

  async function post(body: unknown, control: HTMLButtonElement) {
    control.disabled = true;
    const result = await callDevice(context, endpoint, {
      method: "POST",
      body,
    });
    if (result.kind === "aborted") return;
    control.disabled = false;
    if (result.kind === "ok") return load();
    toastDeviceError(context, result, () => void post(body, control));
  }

  const line = (first: boolean, ...children: (Node | string)[]) => {
    const node = h("div", null, ...children);
    node.style.cssText = `display:flex;align-items:center;gap:12px;padding:10px 16px${first ? "" : ";border-top:1px solid var(--border)"}`;
    return node;
  };

  function draw(reply: OwnersReply) {
    clearInterval(tick);
    const parts: HTMLElement[] = [];
    if (reply.pairing) {
      const deadline = Date.now() + reply.pairing.expires_in * 1000;
      const left = () => Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
      const countdown = h("span", { class: "bc-mono" }, clock(left()));
      const code = h(
        "span",
        { class: "bc-page-title bc-mono" },
        options.command
          ? options.command(reply.pairing.code)
          : reply.pairing.code,
      );
      code.style.overflowWrap = "anywhere";
      const text = h(
        "span",
        null,
        h("span", { class: "bc-small bc-muted" }, pick(options.how, lang)),
        code,
        h("span", { class: "bc-small bc-muted" }, `${s.expiresIn} `, countdown),
      );
      text.style.cssText =
        "flex:1 1 220px;display:flex;flex-direction:column;gap:4px;min-width:0";
      const rotate = button(s.rotate, lang, {
        variant: "outline",
        size: "sm",
        icon: ICON_REFRESH,
        onClick: () => void post({ rotate: true }, rotate),
      });
      const head = h("div", null, text, rotate);
      head.style.cssText =
        "display:flex;flex-wrap:wrap;align-items:center;gap:16px;padding:16px";
      parts.push(head);
      if (reply.pairing.expires_in > 0)
        tick = setInterval(() => {
          const seconds = left();
          countdown.textContent = clock(seconds);
          if (seconds > 0) return;
          clearInterval(tick);
          void load();
        }, 1000);
    }
    for (const owner of reply.owners) {
      const name = owner.label || owner.id;
      const title = h("span", { class: "bc-option-title" }, name);
      if (!owner.label) title.classList.add("bc-mono");
      const text = h(
        "span",
        null,
        title,
        owner.label
          ? h("span", { class: "bc-mono bc-small bc-muted" }, owner.id)
          : null,
      );
      text.style.cssText =
        "flex:1 1 auto;min-width:0;display:flex;flex-direction:column;gap:2px;overflow-wrap:anywhere";
      const remove = button(s.remove, lang, {
        variant: "outline",
        size: "sm",
        onClick: () => void post({ remove: owner.id }, remove),
      });
      remove.setAttribute("aria-label", `${s.remove} ${name}`);
      parts.push(line(parts.length === 0, text, remove));
    }
    if (reply.owners.length === 0) {
      const empty = line(parts.length === 0, s.empty);
      empty.className = "bc-small bc-muted";
      parts.push(empty);
    }
    card.replaceChildren(...parts);
  }

  async function load() {
    const mine = ++seq;
    const result = await callDevice<OwnersReply>(context, endpoint);
    if (mine !== seq || result.kind !== "ok") return;
    const reply = result.data;
    if (!reply || !Array.isArray(reply.owners)) return;
    count = reply.owners.length;
    draw(reply);
    show(element, true);
  }

  return {
    element,
    load,
    hide: () => {
      seq++;
      clearInterval(tick);
      count = null;
      show(element, false);
    },
    count: () => count,
  };
}

export interface ChannelInboundOptions {
  /** The channel's config path (`/api/gateway/telegram`). */
  endpoint: string;
  /** The channel's name in the no-slot alert. */
  channel: Text;
  /** The modes the channel offers (default {@link CHANNEL_MODES}). */
  modes?: readonly ChannelMode[];
  /** Where to send the binding code from. */
  how: Text;
  /** What to send for a code; the bare code by default. */
  command?: (code: string) => string;
  /** How often to read the channel while it receives (default 5 s). */
  pollMs?: number;
  /** Runs with each channel state the rows show, so the page can react to it too. */
  onStatus?: (status: ChannelStatus | null) => void;
}

export interface ChannelInbound {
  /** The 「模式」 and 「授权账号」 rows, hidden until the channel is configured. */
  rows: [HTMLElement, HTMLElement];
  /** Puts the rows into `form` before its 「高级」 fold (or its footer when it has none). */
  attach(form: Pick<SettingsForm, "element" | "footer">): void;
  /** Shows a channel state the page read itself (the mount's `readChannel`). */
  apply(status: ChannelStatus | null): void;
  /** Reads `GET <endpoint>` again and shows it; call it after a save succeeds. */
  refresh(): Promise<ChannelStatus | null>;
}

/**
 * The mode and allowed-accounts rows of a channel page, wired to the device: they show while the
 * channel is configured; a mode change reads the channel again and refreshes the shell's status;
 * while the channel is in `send_receive` it is read every `pollMs` so the receive badge stays live
 * (and the shell's status follows a change); a change in the account count reads the list again.
 */
export function channelInbound(
  context: PortalContext,
  options: ChannelInboundOptions,
): ChannelInbound {
  const pollMs = options.pollMs ?? 5_000;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  let last: ChannelStatus | null = null;
  const mode = modeRow(
    {
      endpoint: options.endpoint,
      channel: options.channel,
      modes: options.modes,
      onChange: () => void refresh().then(() => void context.refreshStatus()),
    },
    context,
  );
  const accounts = accountsRow(
    { endpoint: options.endpoint, how: options.how, command: options.command },
    context,
  );
  context.signal.addEventListener("abort", () => clearTimeout(timer), {
    once: true,
  });

  function apply(status: ChannelStatus | null, polled = false) {
    clearTimeout(timer);
    seq++;
    const before = last?.receive?.state;
    last = status;
    mode.update(status);
    options.onStatus?.(status);
    if (!status?.configured) {
      accounts.hide();
      return;
    }
    const owners = status.owners?.count;
    if (
      accounts.count() === null ||
      (owners !== undefined && owners !== accounts.count())
    )
      void accounts.load();
    if (polled && status.receive?.state !== before)
      void context.refreshStatus();
    if (status.mode === "send_receive" && !context.signal.aborted)
      timer = setTimeout(() => void refresh(true), pollMs);
  }

  async function refresh(polled = false): Promise<ChannelStatus | null> {
    const mine = ++seq;
    const result = await callDevice<ChannelStatus>(context, options.endpoint);
    if (mine !== seq || context.signal.aborted) return last;
    const status =
      result.kind === "ok" && typeof result.data?.configured === "boolean"
        ? result.data
        : null;
    // a missed poll keeps what the page shows and asks again
    if (!status && last) {
      apply(last);
      return last;
    }
    apply(status, polled);
    return status;
  }

  return {
    rows: [mode.element, accounts.element],
    attach(form) {
      const fold = form.element
        .querySelector(".bc-disclosure")
        ?.closest<HTMLElement>(".bc-row");
      (fold && fold.parentElement === form.element ? fold : form.footer).before(
        mode.element,
        accounts.element,
      );
    },
    apply: (status) => apply(status),
    refresh: () => refresh(),
  };
}
