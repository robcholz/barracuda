import {
  ICON_ARROW_DOWN,
  ICON_ARROW_UP,
  ICON_CHECK,
  ICON_CHEVRON_RIGHT,
  ICON_CIRCLE_ALERT,
  ICON_CIRCLE_X,
  ICON_CLOCK,
  ICON_COPY,
  ICON_DOWNLOAD,
  ICON_FILE,
  ICON_LIGHTBULB,
  ICON_LOADER,
  ICON_MESSAGE_SQUARE_DASHED,
  ICON_PENCIL,
  ICON_REFRESH,
  ICON_REPLY,
  ICON_SHIELD_ALERT,
  ICON_WIFI_OFF,
  ICON_WRENCH,
  ICON_X,
  MARK_TILE,
  copyText,
  definePage,
  h,
  icon,
  mark,
  type PortalContext,
} from "../../../captive-portal/resources/web/ui";
import { spinningMark } from "./mark";
import { CHAT_CSS } from "./style";

/**
 * Web chat over the bridge's WebSocket `/ws/message`. The page sends `WebClientFrame` JSON
 * (`{ text }`; a reply carries its quote inside the text, see {@link withQuote}) and renders the SSE-formatted frames the bridge pushes: message
 * lifecycle (`message.start`/`delta`/`end`/`edit`/`delete`/`reaction`), the Agent's semantic
 * events inside `message.event`, attachments, typing and `stream.lagged`. Live events only.
 *
 * The device runs one turn at a time. A message written while a turn runs waits in the page's
 * queue, where it can still be edited or removed, and goes out when the turn ends.
 */

const STRINGS = {
  zh: {
    log: "聊天记录",
    sent: "已发送",
    reasoning: "思考过程",
    thinking: "思考中",
    /** 「思考了 `3` 秒」: the number is mono. */
    thoughtA: "思考了 ",
    thoughtB: " 秒",
    ok: "成功",
    failed: "失败",
    running: "运行中",
    args: "参数",
    output: "输出",
    reply: "回复",
    copy: "复制",
    copied: "已复制",
    edited: "已编辑",
    approvalTitle: "需要你的允许",
    approvalTool: "工具",
    approvalArgs: "参数",
    approvalReason: "理由",
    allow: "允许",
    deny: "拒绝",
    approvalHint: "或直接回复",
    answering: "正在答复",
    answerPh: "回复这次请求…",
    emptyTitle: "临时会话",
    emptyBody: "在浏览器里直接和设备对话；刷新页面后从空白开始。",
    suggestions: [
      "今天适合骑车吗？",
      "总结一下我今天的日程",
      "提醒我 6 点下班",
    ],
    incomplete: "未完成 · 连接中断",
    lagged: "错过 {n} 个事件",
    turnError: "这一轮出错",
    offline: "连接已中断",
    live: "已连接",
    lost: "已断开",
    /** After the count in mono: 「`1` 条消息未确认，重新连接后再发送」. */
    unconfirmed: (_n: number) => " 条消息未确认，重新连接后再发送",
    reconnect: "重新连接",
    reconnected: "已重新连接",
    label: "消息",
    placeholder: "给设备发一条消息",
    send: "发送",
    enqueue: "加入队列",
    queued: "排队中",
    editQueued: "编辑",
    removeQueued: "移除",
    jump: "回到最新",
    replying: "回复 Barracuda",
    cancelReply: "取消回复",
    session: "临时会话",
    sessionTip: "只显示连接后的消息，刷新页面后清空",
    download: "下载",
    receiving: "接收中",
    received: "已接收",
    mediaFailed: "接收失败",
    reaction: "回应",
    tooLong: "消息太长，请缩短后发送。",
    notSent: "消息未发出",
    busy: "发送队列繁忙，请稍后再试。",
    unreadable: "收到无法识别的消息",
  },
  en: {
    log: "Conversation",
    sent: "Sent",
    reasoning: "Reasoning",
    thinking: "Thinking",
    thoughtA: "Thought for ",
    thoughtB: "s",
    ok: "Succeeded",
    failed: "Failed",
    running: "Running",
    args: "Arguments",
    output: "Output",
    reply: "Reply",
    copy: "Copy",
    copied: "Copied",
    edited: "Edited",
    approvalTitle: "Needs your permission",
    approvalTool: "Tool",
    approvalArgs: "Arguments",
    approvalReason: "Reason",
    allow: "Allow",
    deny: "Deny",
    approvalHint: "Or reply in your own words",
    answering: "Answering",
    answerPh: "Reply to this request…",
    emptyTitle: "Temporary session",
    emptyBody:
      "Talk to the device right in the browser; a reload starts from blank.",
    suggestions: [
      "Is it a good day for a ride?",
      "Summarize today's schedule",
      "Remind me to leave at 6",
    ],
    incomplete: "Incomplete · connection lost",
    lagged: "Missed {n} events",
    turnError: "This turn failed",
    offline: "Disconnected",
    live: "Connected",
    lost: "Disconnected",
    unconfirmed: (n: number) =>
      ` ${n === 1 ? "message" : "messages"} unconfirmed; send again after reconnecting`,
    reconnect: "Reconnect",
    reconnected: "Reconnected",
    label: "Message",
    placeholder: "Message the device",
    send: "Send",
    enqueue: "Add to queue",
    queued: "Queued",
    editQueued: "Edit",
    removeQueued: "Remove",
    jump: "Jump to latest",
    replying: "Replying to Barracuda",
    cancelReply: "Cancel reply",
    session: "Temporary session",
    sessionTip:
      "Only messages since you connected; cleared when the page reloads",
    download: "Download",
    receiving: "receiving",
    received: "received",
    mediaFailed: "failed",
    reaction: "Reaction",
    tooLong: "This message is too long; shorten it to send.",
    notSent: "Message not sent",
    busy: "The send queue is full; try again shortly.",
    unreadable: "Received a message this page can't read",
  },
};

/** The bridge's limits as the previous page enforced them. */
const MAX_BYTES = 1024;
/** A reply carries at most this many UTF-8 bytes of the message it quotes, on top of its own text. */
const MAX_QUOTE = 512;
const MAX_ITEMS = 100;
const MAX_TEXT = 65536;
const MAX_FRAME = 262144;
const MAX_MEDIA = 8 * 1024 * 1024;
/** The composer grows with its text up to this height, then scrolls. */
const MAX_INPUT = 200;
/**
 * A sent message whose turn has not started after this long, with nothing else heard, no longer
 * holds the queue: the device queues it anyway, so the page stops waiting on a turn it may miss.
 */
const STALL_MS = 30000;

const fill = (text: string, map: Record<string, string | number>) =>
  text.replace(/\{(\w+)\}/g, (_, key: string) => String(map[key]));
const clip = (text: string) =>
  text.length > MAX_TEXT ? text.slice(-MAX_TEXT) : text;
const pad = (n: number) => String(n).padStart(2, "0");

type Data = Record<string, unknown>;
const str = (value: unknown) => (typeof value === "string" ? value : "");

/** Shows or hides a node, whatever display its inline style or class gives it. */
function show(node: HTMLElement, on: boolean) {
  node.hidden = !on;
  // showing undoes only what hiding did, so a node's own inline display survives
  if (on) {
    if (node.style.display === "none")
      node.style.display = node.dataset.display ?? "";
  } else {
    if (node.style.display !== "none")
      node.dataset.display = node.style.display;
    node.style.display = "none";
  }
}

/**
 * One fold (reasoning or a tool card): a button that shows and hides `panel`. Its trailing chevron
 * turns with `aria-expanded`, in the design system's CSS.
 */
function fold(button: HTMLButtonElement, panel: HTMLElement, open: boolean) {
  const set = (value: boolean) => {
    button.setAttribute("aria-expanded", String(value));
    show(panel, value);
  };
  button.addEventListener("click", () =>
    set(button.getAttribute("aria-expanded") !== "true"),
  );
  set(open);
  return set;
}

/** An icon-only button named by its tooltip, as the design system's Button card asks. */
function iconButton(
  label: string,
  paths: string,
  onclick: () => void,
  className = "bc-icon-button",
  type: "button" | "submit" = "button",
) {
  const tip = h("span", { class: "bc-tooltip", "aria-hidden": "true" }, label);
  const button = h(
    "button",
    { class: className, type, "aria-label": label, onclick },
    icon(paths),
    tip,
  );
  return {
    button,
    /** Renames the button (its label and tooltip) and swaps its icon. */
    set(next: string, nextPaths = paths) {
      button.setAttribute("aria-label", next);
      tip.textContent = next;
      button.firstElementChild!.replaceWith(icon(nextPaths));
    },
  };
}

interface Tool {
  card: HTMLElement;
  name: HTMLElement;
  summary: HTMLElement;
  args: HTMLElement;
  out: HTMLElement;
  status: HTMLElement;
  open: (value: boolean) => void;
}

interface Request {
  card: HTMLElement;
  tool: HTMLElement;
  args: HTMLElement;
  reason: HTMLElement;
  actions: HTMLElement;
  owner: Agent;
}

/**
 * One open text run. Text the device sends lands in `pending` and flows into `text` a few
 * characters a frame, so a reply streams in smoothly rather than in bursts; `done` runs when the
 * run closes.
 */
interface Run {
  stream: string;
  text: Text;
  node: HTMLElement;
  pending: string;
  done?: () => void;
}

/** A turn of Agent messages under one author line, whose mark spins while any of them runs. */
interface Group {
  node: HTMLElement;
  author: HTMLElement;
  spin: { canvas: HTMLCanvasElement; stop(): void } | null;
}

interface Agent {
  id: string;
  kind: string;
  root: HTMLElement;
  group: Group;
  run: Run | null;
  tool: Tool | null;
  request: Request | null;
  footer: HTMLElement | null;
  reaction: HTMLElement;
  ended: boolean;
  media?: Media;
}

interface Media {
  chunks: Uint8Array[];
  size: number;
  kept: boolean;
  meta: HTMLElement;
  slot: HTMLElement;
  filename: string;
  mime: string;
}

interface Sent {
  root: HTMLElement;
  reaction: HTMLElement;
  confirmed: boolean;
}

/** A message waiting in the page for the running turn to end. */
interface Queued {
  text: string;
  replyTo: { id: string; text: string } | null;
}

/**
 * The text a reply sends: the quoted message as XML ahead of the user's own words, so the device
 * needs no notion of replies. The quote is escaped so it cannot close its element early, and cut
 * to {@link MAX_QUOTE} bytes.
 */
function withQuote(text: string, quoted: string) {
  let body = "";
  let kept = "";
  let size = 0;
  for (const char of quoted) {
    const part =
      char === "&"
        ? "&amp;"
        : char === "<"
          ? "&lt;"
          : char === ">"
            ? "&gt;"
            : char;
    const point = char.codePointAt(0)!;
    size +=
      part.length > 1 || point < 0x80
        ? part.length
        : point < 0x800
          ? 2
          : point < 0x10000
            ? 3
            : 4;
    // a cut quote ends in an ellipsis, so it keeps what fits beside one
    if (size > MAX_QUOTE) return `<quote>${kept}…</quote>\n${text}`;
    body += part;
    if (size <= MAX_QUOTE - 3) kept = body;
  }
  return `<quote>${body}</quote>\n${text}`;
}

export const mount = definePage((context: PortalContext) => {
  const { lang, signal } = context;
  const t = STRINGS[lang];
  const doc = document;
  // the page's own rules, for as long as it is mounted
  const style = h("style", null, CHAT_CSS);
  doc.head.append(style);

  // ---- layout
  const column = h("div", {
    style:
      "flex:1 1 auto;width:100%;max-width:760px;margin:0 auto;display:flex;flex-direction:column;gap:var(--space-6)",
  });
  const log = h(
    "div",
    {
      class: "bc-chat-log",
      role: "log",
      "aria-label": t.log,
      "aria-live": "polite",
      // a column, so the conversation fills the height and centres the empty state
      style: "display:flex;flex-direction:column",
    },
    column,
  );
  // a temporary session says so at the conversation's head, pinned while the history scrolls
  const head = h(
    "div",
    { class: "bc-chat-head" },
    h(
      "span",
      {
        class: "bc-badge",
        tabindex: 0,
        "aria-describedby": "imessage-web-session-tip",
      },
      icon(ICON_MESSAGE_SQUARE_DASHED),
      t.session,
      h(
        "span",
        {
          class: "bc-tooltip",
          role: "tooltip",
          id: "imessage-web-session-tip",
        },
        t.sessionTip,
      ),
    ),
  );
  // the fresh conversation: the EmptyState with the page's laptop figure, over a mid-page composer
  const empty = h(
    "div",
    { class: "bc-empty", style: "gap:12px" },
    h("hl-figure", {
      name: "imessage-web",
      "aria-hidden": "true",
      style: "width:200px;max-width:100%",
    }),
    h("h2", { class: "bc-title" }, t.emptyTitle),
    h(
      "p",
      { class: "bc-small bc-muted", style: "margin:0;max-width:420px" },
      t.emptyBody,
    ),
  );
  const suggestions = h(
    "div",
    {
      style:
        "display:flex;flex-wrap:wrap;justify-content:center;gap:8px;padding-top:8px",
    },
    t.suggestions.map((text) =>
      h(
        "button",
        {
          class: "bc-button bc-button--outline bc-button--sm",
          type: "button",
          onclick: () => {
            input.value = text;
            changed();
            submit();
          },
        },
        text,
      ),
    ),
  );
  // what the device is doing while nothing streams: 「思考中」, after a spinning author line
  const thinking = h(
    "span",
    {
      class: "bc-small bc-muted",
      style: "display:flex;align-items:center;gap:8px",
    },
    h("span", { class: "bc-shimmer" }, t.thinking),
  );
  const dots = h(
    "span",
    { class: "bc-typing", "aria-hidden": "true" },
    h("i"),
    h("i"),
    h("i"),
  );
  const caret = h("span", { class: "bc-caret", "aria-hidden": "true" });

  const input = h("textarea", {
    id: "imessage-web-message",
    rows: 1,
    placeholder: t.placeholder,
  });
  const sendButton = iconButton(
    t.send,
    ICON_ARROW_UP,
    () => {},
    "bc-button bc-button--icon",
    "submit",
  );
  sendButton.button.disabled = true;
  // a ruled bar: the icon carries the warning colour, the text stays foreground
  const answeringText = h("span");
  const answering = h(
    "div",
    { class: "bc-composer__bar bc-composer__bar--rule" },
    icon(ICON_SHIELD_ALERT, undefined, "bc-warning"),
    answeringText,
  );
  show(answering, false);
  // the queue: a counting bar, then one bar per waiting message with its edit and remove actions
  const queueList = h("div", { role: "list", "aria-label": t.queued });
  show(queueList, false);
  const quote = h("span", { class: "bc-composer__text" });
  const replyBar = h(
    "div",
    { class: "bc-composer__bar" },
    icon(ICON_REPLY, undefined, "bc-muted"),
    h("span", { class: "bc-muted", style: "flex:none" }, t.replying),
    quote,
    iconButton(t.cancelReply, ICON_X, () => setReply(null)).button,
  );
  show(replyBar, false);
  const composer = h(
    "div",
    { class: "bc-composer" },
    answering,
    queueList,
    replyBar,
    h(
      "label",
      {
        for: "imessage-web-message",
        style:
          "position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%)",
      },
      t.label,
    ),
    h(
      "div",
      { class: "bc-composer__row" },
      input,
      h("div", { class: "bc-composer__actions" }, sendButton.button),
    ),
  );
  const invalid = h("span", {
    class: "bc-hint bc-hint--error",
    role: "alert",
    id: "imessage-web-message-error",
  });
  show(invalid, false);
  const offlineCount = h("span");
  const offline = h(
    "div",
    { class: "bc-alert bc-alert--error bc-alert--compact", role: "alert" },
    icon(ICON_WIFI_OFF),
    h(
      "span",
      { class: "bc-small", style: "flex:1 1 auto" },
      h("span", { class: "bc-alert__title" }, t.offline),
      offlineCount,
    ),
    h(
      "button",
      {
        class: "bc-button bc-button--outline bc-button--sm",
        type: "button",
        onclick: () => connect(true),
      },
      icon(ICON_REFRESH),
      t.reconnect,
    ),
  );
  show(offline, false);
  const jump = iconButton(
    t.jump,
    ICON_ARROW_DOWN,
    () => {
      stuck = true;
      follow();
    },
    "bc-button bc-button--outline bc-button--icon bc-chat-jump",
  ).button;
  show(jump, false);
  const form = h(
    "form",
    {
      class: "bc-chat-dock",
      onsubmit: (event: Event) => {
        event.preventDefault();
        submit();
      },
    },
    h(
      "div",
      {
        style:
          "position:relative;width:100%;max-width:760px;margin:0 auto;display:flex;flex-direction:column;gap:8px",
      },
      jump,
      offline,
      composer,
      invalid,
      suggestions,
    ),
  );
  const page = h(
    "div",
    { style: "flex:1 1 auto;display:flex;flex-direction:column;min-width:0" },
    head,
    log,
    form,
  );

  /** The fresh conversation holds the composer mid-page; once it has messages, the head says what it is. */
  function setFresh(on: boolean) {
    page.classList.toggle("bc-chat--fresh", on);
    column.style.flex = on ? "0 0 auto" : "1 1 auto";
    show(head, !on);
    show(suggestions, on);
  }

  // ---- conversation state
  const items: { node: HTMLElement; forget(): void }[] = [];
  const agents = new Map<string, Agent>();
  let sent = new Map<string, Sent>();
  const urls = new Set<string>();
  const groups = new Set<Group>();
  let group: Group | null = null;
  let request: Request | null = null;
  /** The message that answered a permission request, until the asking turn goes on. */
  let answered: { owner: Agent; entry: Sent } | null = null;
  let replyTo: { id: string; text: string } | null = null;
  /** Messages the device is running or streaming (started, not ended). */
  const active = new Set<Agent>();
  /** Sent messages whose turn has not started yet. */
  const awaiting = new Set<string>();
  const queue: Queued[] = [];
  let typingOn = false;
  let stall: ReturnType<typeof setTimeout> | undefined;
  /** The turn the device has been asked for and not yet begun: an author line over 「思考中」. */
  const pending: Group = {
    node: h("div", { class: "bc-turn bc-turn--new" }),
    author: author(),
    spin: null,
  };
  pending.node.append(pending.author);

  function add(node: HTMLElement, forget = () => {}, into?: HTMLElement) {
    (into ?? column).append(node);
    if (!into) group = null;
    items.push({ node, forget });
    while (items.length > MAX_ITEMS) {
      const old = items.shift()!;
      const parent = old.node.parentElement;
      old.node.remove();
      old.forget();
      if (parent && parent !== column && parent.childElementCount <= 1)
        parent.remove();
    }
    if (empty.isConnected) {
      empty.remove();
      setFresh(false);
    }
  }

  function author() {
    return h(
      "span",
      { class: "bc-meta bc-meta--author" },
      mark(MARK_TILE, 16),
      "Barracuda",
    );
  }

  /** Spins a turn's mark while it runs, and rests it when it ends; without WebGL it stays still. */
  function spin(target: Group, on: boolean) {
    if (on === !!target.spin) return;
    if (on) {
      const spinning = spinningMark();
      if (!spinning) return;
      target.spin = spinning;
      target.author.firstElementChild!.replaceWith(
        h("span", { class: "bc-mark-tile" }, spinning.canvas),
      );
    } else {
      target.spin!.stop();
      target.spin = null;
      target.author.firstElementChild!.replaceWith(mark(MARK_TILE, 16));
    }
  }

  function reactionBadge() {
    const badge = h("span", { class: "bc-badge", "aria-label": t.reaction });
    show(badge, false);
    return badge;
  }

  function addUser(text: string, id: string, quoted: string | null) {
    const reaction = reactionBadge();
    const node = h(
      "div",
      { class: "bc-turn bc-turn--user bc-turn--new" },
      quoted === null
        ? null
        : h(
            "span",
            {
              class: "bc-quote",
              style: "display:flex;align-items:center;gap:8px;max-width:100%",
            },
            icon(ICON_REPLY),
            h("span", { class: "bc-composer__text" }, quoted),
          ),
      h("p", { class: "bc-bubble" }, text),
      h("span", { class: "bc-meta" }, reaction, t.sent),
    );
    const record = { root: node, reaction, confirmed: false };
    const map = sent;
    map.set(id, record);
    add(node, () => map.delete(id));
    return record;
  }

  function agent(id: string, kind = "reply"): Agent {
    const known = agents.get(id);
    if (known) return known;
    if (!group || !group.node.isConnected) {
      const opened: Group = {
        node: h("div", { class: "bc-turn bc-turn--new" }),
        author: author(),
        spin: null,
      };
      opened.node.append(opened.author);
      groups.add(opened);
      column.append(opened.node);
      group = opened;
    }
    // one message of the turn: its parts on the turn's own 8px rhythm
    const root = h("div", {
      style: "display:flex;flex-direction:column;gap:8px",
    });
    const record: Agent = {
      id,
      kind,
      root,
      group,
      run: null,
      tool: null,
      request: null,
      footer: null,
      reaction: reactionBadge(),
      ended: false,
    };
    agents.set(id, record);
    // a message first heard mid-stream is running too, until its end
    if (kind !== "media") active.add(record);
    const owner = group;
    add(root, () => forget(record), owner.node);
    group = owner;
    return record;
  }

  function forget(record: Agent) {
    agents.delete(record.id);
    active.delete(record);
    if (record.run) close(record);
    if (request?.owner === record) setRequest(null);
    if (replyTo?.id === record.id) setReply(null);
  }

  // ---- smooth streaming
  const flowing = new Set<Run>();
  let frame = 0;

  /** Whether text should land at once: reduced motion, or a hidden tab whose frames pause. */
  function instant() {
    return (
      !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ||
      doc.hidden
    );
  }

  function flow(run: Run, text: string) {
    run.pending += text;
    if (instant() || run.pending.length > MAX_TEXT) return settle(run);
    flowing.add(run);
    if (!frame) frame = requestAnimationFrame(tick);
  }

  /** Reveals a share of each run's backlog: steady when it is short, catching up when it is long. */
  function tick() {
    frame = 0;
    for (const run of flowing) {
      let n = Math.min(
        run.pending.length,
        Math.max(2, Math.ceil(run.pending.length / 10)),
      );
      // never split a surrogate pair across frames
      const code = run.pending.charCodeAt(n - 1);
      if (code >= 0xd800 && code <= 0xdbff) n += 1;
      run.text.data = clip(run.text.data + run.pending.slice(0, n));
      run.pending = run.pending.slice(n);
      if (!run.pending) flowing.delete(run);
    }
    if (flowing.size) frame = requestAnimationFrame(tick);
    follow();
  }

  function settle(run: Run) {
    if (run.pending) run.text.data = clip(run.text.data + run.pending);
    run.pending = "";
    flowing.delete(run);
  }

  /** Closes the message's open run: its text lands in full and its `done` runs. */
  function close(record: Agent) {
    const run = record.run;
    if (!run) return;
    settle(run);
    record.run = null;
    run.done?.();
  }

  /** Appends text to the message's run for `stream`, opening a new run when the stream changes. */
  function write(record: Agent, stream: string, text: string) {
    if (!text) return;
    if (record.run?.stream !== stream) {
      close(record);
      let done: (() => void) | undefined;
      let node: HTMLElement;
      if (stream === "reasoning") ({ node, done } = reasoning(record));
      else {
        node = h("p", {
          class:
            stream === "notice" || stream === "tool"
              ? "bc-small bc-muted"
              : "bc-reply",
          style: stream === "notice" || stream === "tool" ? "margin:0" : null,
        });
        if (stream === "tool") node.append(icon(ICON_WRENCH), " ");
      }
      const content = doc.createTextNode("");
      node.append(content);
      if (stream !== "reasoning") put(record, node);
      record.run = { stream, text: content, node, pending: "", done };
    }
    const run = record.run!;
    flow(run, text);
    if (stream === "output" && !record.ended) run.node.append(caret);
  }

  /** Adds a part to a message, above its footer. */
  function put(record: Agent, node: HTMLElement) {
    record.root.insertBefore(
      node,
      record.footer?.parentElement ? record.footer : null,
    );
  }

  /**
   * Reasoning: open under a shimmering 「思考中」 while it streams, then folded away under
   * 「思考了 N 秒」 unless the reader opened or closed it meanwhile.
   */
  function reasoning(record: Agent) {
    const panel = h("p", {
      class: "bc-quote",
      style: "margin-top:8px;white-space:pre-wrap;overflow-wrap:anywhere",
    });
    const label = h("span", { class: "bc-shimmer" }, t.thinking);
    const button = h(
      "button",
      { class: "bc-fold-link", type: "button" },
      icon(ICON_LIGHTBULB),
      label,
      icon(ICON_CHEVRON_RIGHT),
    );
    const set = fold(button, panel, true);
    let touched = false;
    button.addEventListener("click", () => (touched = true));
    const started = Date.now();
    put(record, h("div", null, button, panel));
    const done = () => {
      const seconds = Math.round((Date.now() - started) / 1000);
      label.removeAttribute("class");
      label.replaceChildren(
        ...(seconds >= 1
          ? [
              t.thoughtA,
              h("span", { class: "bc-mono" }, String(seconds)),
              t.thoughtB,
            ]
          : [t.reasoning]),
      );
      if (!touched) set(false);
    };
    return { node: panel, done };
  }

  /**
   * A tool-call record (Card): the fold names the tool and its arguments in one line; open, a dense
   * key-value body holds them in full. It spins 「运行中」 until its result ends.
   */
  function tool(record: Agent): Tool {
    const name = h("span", { class: "bc-tool__name" });
    const summary = h("span", { class: "bc-tool__args" });
    const status = h(
      "span",
      { class: "bc-status bc-muted", style: "flex:none;white-space:nowrap" },
      icon(ICON_LOADER, undefined, "bc-spinner"),
      t.running,
    );
    const button = h(
      "button",
      { class: "bc-fold", type: "button" },
      icon(ICON_WRENCH, undefined, "bc-muted"),
      name,
      summary,
      status,
      icon(ICON_CHEVRON_RIGHT, undefined, "bc-muted"),
    );
    // the values keep the device's line breaks
    const args = h("dd", { class: "bc-mono", style: "white-space:pre-wrap" });
    const out = h("dd", { class: "bc-mono", style: "white-space:pre-wrap" });
    const panel = h(
      "dl",
      { class: "bc-card__body bc-kv bc-kv--dense" },
      h("dt", null, t.args),
      args,
      h("dt", null, t.output),
      out,
    );
    const open = fold(button, panel, false);
    const card = h("div", { class: "bc-card" }, button, panel);
    close(record);
    put(record, card);
    return { card, name, summary, args, out, status, open };
  }

  /** A permission request (Card): head, the tool, arguments and reason, then the answers in the foot. */
  function approval(record: Agent): Request {
    const tool = h("dd", { class: "bc-tool__name" });
    const args = h("code", { class: "bc-code" });
    const reason = h("dd");
    const actions = h(
      "div",
      { class: "bc-card__foot" },
      h(
        "button",
        {
          class: "bc-button bc-button--sm",
          type: "button",
          onclick: () => answer(t.allow),
        },
        icon(ICON_CHECK),
        t.allow,
      ),
      h(
        "button",
        {
          class: "bc-button bc-button--outline bc-button--sm",
          type: "button",
          onclick: () => answer(t.deny),
        },
        icon(ICON_X),
        t.deny,
      ),
      h(
        "span",
        { class: "bc-small bc-muted", style: "flex:1 1 240px" },
        t.approvalHint,
      ),
    );
    const title = `imessage-web-approval-${record.id}`;
    const card = h(
      "section",
      { class: "bc-card", "aria-labelledby": title },
      h(
        "div",
        { class: "bc-card__head" },
        icon(ICON_SHIELD_ALERT, undefined, "bc-warning"),
        h("h3", { id: title, class: "bc-title" }, t.approvalTitle),
      ),
      h(
        "dl",
        { class: "bc-card__body bc-kv bc-kv--dense" },
        h("dt", null, t.approvalTool),
        tool,
        h("dt", null, t.approvalArgs),
        h("dd", null, args),
        h("dt", null, t.approvalReason),
        reason,
      ),
      actions,
    );
    show(actions, false);
    close(record);
    put(record, card);
    return { card, tool, args, reason, actions, owner: record };
  }

  /** Shows the message's footer (its reaction, then Copy and Reply as icon buttons): at its end or while it asks. */
  function showFooter(record: Agent) {
    if (record.kind !== "reply") return;
    if (!record.footer) {
      let reset: ReturnType<typeof setTimeout> | undefined;
      const copy = iconButton(t.copy, ICON_COPY, async () => {
        if (!(await copyText(textOf(record))) || signal.aborted) return;
        copy.set(t.copied, ICON_CHECK);
        clearTimeout(reset);
        reset = setTimeout(() => copy.set(t.copy, ICON_COPY), 1500);
      });
      record.footer = h(
        "div",
        { class: "bc-meta", style: "gap:4px" },
        record.reaction,
        copy.button,
        iconButton(t.reply, ICON_REPLY, () =>
          setReply({ id: record.id, text: textOf(record) }),
        ).button,
      );
    }
    record.root.append(record.footer);
  }

  /** The message's reply text, as Copy and Reply take it. */
  function textOf(record: Agent) {
    return [...record.root.querySelectorAll("p.bc-reply")]
      .map((node) => node.firstChild?.textContent ?? "")
      .join("\n")
      .trim();
  }

  function setReply(target: { id: string; text: string } | null) {
    replyTo = target;
    show(replyBar, !!target);
    quote.textContent = target?.text ?? "";
    if (target) input.focus();
  }

  function setRequest(next: Request | null) {
    if (request && request !== next) {
      show(request.actions, false);
      for (const button of request.actions.querySelectorAll("button"))
        button.disabled = true;
    }
    request = next;
    show(answering, !!next);
    // 「正在答复 · `fs.remove`」: the tool's name is a machine value
    answeringText.replaceChildren(
      ...(next
        ? [
            `${t.answering} · `,
            h("span", { class: "bc-mono" }, next.tool.textContent),
          ]
        : []),
    );
    input.placeholder = next ? t.answerPh : t.placeholder;
  }

  /** A failed turn: an error alert written into the conversation, a record in the log rather than a page alert. */
  function errorRecord(title: string, text: string) {
    add(
      h(
        "div",
        { class: "bc-alert bc-alert--error bc-turn--new", role: "alert" },
        icon(ICON_CIRCLE_ALERT),
        h(
          "span",
          { class: "bc-alert__body" },
          h("span", { class: "bc-alert__title" }, title),
          text
            ? h(
                "span",
                { class: "bc-small", style: "overflow-wrap:anywhere" },
                text,
              )
            : null,
        ),
      ),
    );
  }

  function separator(text: string) {
    add(h("div", { role: "separator", class: "bc-separator" }, text));
  }

  // ---- semantic Agent events inside one reply message
  function semantic(record: Agent, type: string, payload: Data) {
    const text = str(payload.text);
    if (answered?.owner === record) {
      // the asking turn went on after the answer, so the device received it
      answered.entry.confirmed = true;
      answered = null;
    }
    switch (type) {
      case "reasoning_delta":
        return write(record, "reasoning", text);
      case "output_delta":
      case "effect_output_delta":
        return write(record, "output", text);
      case "reasoning_ended":
      case "output_ended":
      case "effect_output_ended":
        caret.remove();
        return close(record);
      case "tool_result_started":
        record.tool = tool(record);
        return;
      case "tool_name_delta":
      case "tool_arguments_delta":
      case "tool_output_delta": {
        const card = record.tool;
        if (!card) return;
        const target =
          type === "tool_name_delta"
            ? card.name
            : type === "tool_arguments_delta"
              ? card.args
              : card.out;
        target.textContent = clip((target.textContent ?? "") + text);
        card.summary.textContent = card.args.textContent;
        return;
      }
      case "tool_result_ended": {
        const card = record.tool;
        if (!card) return;
        const ok = payload.ok === true;
        card.status.className = `bc-status ${ok ? "bc-success" : "bc-destructive"}`;
        card.status.replaceChildren(
          icon(ok ? ICON_CHECK : ICON_CIRCLE_X),
          ok ? t.ok : t.failed,
        );
        if (!ok) card.out.classList.add("bc-destructive");
        card.open(!ok);
        record.tool = null;
        return;
      }
      case "input_request_started":
        record.request = approval(record);
        return;
      case "input_request_tool_name_delta":
      case "input_request_arguments_delta":
      case "input_request_reason_delta": {
        const card = record.request;
        if (!card) return;
        const target =
          type === "input_request_tool_name_delta"
            ? card.tool
            : type === "input_request_arguments_delta"
              ? card.args
              : card.reason;
        target.textContent = clip((target.textContent ?? "") + text);
        return;
      }
      case "input_requested":
        if (!record.request) return;
        show(record.request.actions, true);
        setRequest(record.request);
        return showFooter(record);
      case "turn_error":
      case "session_error":
        caret.remove();
        close(record);
        return errorRecord(t.turnError, str(payload.message));
    }
  }

  // ---- bridge frames
  function receive(frame: unknown) {
    if (typeof frame !== "string" || frame.length > MAX_FRAME)
      throw new Error("invalid frame");
    for (const block of frame.replaceAll("\r\n", "\n").split("\n\n")) {
      if (!block.trim()) continue;
      const lines = block.split("\n");
      const event = lines
        .find((line) => line.startsWith("event:"))
        ?.slice(6)
        .trim();
      const data: unknown = JSON.parse(
        lines
          .filter((line) => line.startsWith("data:"))
          .map((line) => line.slice(5).trimStart())
          .join("\n"),
      );
      if (!data || typeof data !== "object") throw new Error("invalid data");
      handle(event ?? "", data as Data);
    }
  }

  function handle(event: string, value: Data) {
    if (event === "stream.lagged")
      return separator(fill(t.lagged, { n: Number(value.missed) || 0 }));
    if (event === "conversation.typing") {
      typingOn = value.typing === true;
      return;
    }
    const id = str(value.message_id);
    if (!id) return;
    const known = agents.get(id);
    switch (event) {
      case "message.start": {
        agent(id, str(value.kind) || "reply");
        const replied = str(value.reply_to);
        // the oldest waiting message's turn has started
        awaiting.delete(replied);
        const target = sent.get(replied);
        if (target) target.confirmed = true;
        return;
      }
      case "message.delta": {
        const record = agent(id);
        const stream =
          record.kind === "reasoning" ||
          record.kind === "tool" ||
          record.kind === "notice"
            ? record.kind
            : "output";
        return write(record, stream, str(value.delta));
      }
      case "message.event": {
        const payload = value.payload;
        return semantic(
          agent(id),
          str(value.type),
          payload && typeof payload === "object" ? (payload as Data) : {},
        );
      }
      case "message.end": {
        if (!known) return;
        known.ended = true;
        active.delete(known);
        caret.remove();
        close(known);
        if (request?.owner === known) setRequest(null);
        // the device's reason is internal (no plumbing on screen): the line says what happened
        if (str(value.error))
          put(
            known,
            h(
              "span",
              { class: "bc-status bc-destructive" },
              icon(ICON_CIRCLE_ALERT),
              t.incomplete,
            ),
          );
        return showFooter(known);
      }
      case "message.edit": {
        if (!known) return;
        close(known);
        for (const node of known.root.querySelectorAll("p.bc-reply"))
          node.remove();
        write(known, "output", str(value.text) || " ");
        caret.remove();
        const run = known.run!;
        settle(run);
        run.node.append(
          h("span", { class: "bc-caption bc-muted" }, ` · ${t.edited}`),
        );
        known.run = null;
        return;
      }
      case "message.delete": {
        const user = sent.get(id);
        const node = known?.root ?? user?.root;
        const index = items.findIndex((item) => item.node === node);
        if (index < 0) return;
        const [item] = items.splice(index, 1);
        const parent = item.node.parentElement;
        item.node.remove();
        item.forget();
        if (parent && parent !== column && parent.childElementCount <= 1) {
          if (parent === group?.node) group = null;
          parent.remove();
        }
        if (!items.length) {
          column.prepend(empty);
          setFresh(true);
        }
        return;
      }
      case "message.reaction": {
        const badge = known?.reaction ?? sent.get(id)?.reaction;
        if (!badge) return;
        const reaction = str(value.reaction);
        badge.textContent = reaction;
        show(badge, !!reaction);
        if (known) showFooter(known);
        return;
      }
      case "message.file":
      case "message.image":
      case "message.audio":
      case "message.video":
        return media(id, value);
    }
  }

  function media(id: string, value: Data) {
    const phase = str(value.phase);
    if (phase === "start") {
      const record = agent(id, "media");
      const filename = str(value.filename);
      const mime = str(value.mime_type);
      const meta = h("span", { class: "bc-caption bc-muted" });
      const slot = h("span", { style: "flex:none;display:inline-flex" });
      record.media = {
        chunks: [],
        size: 0,
        kept: true,
        meta,
        slot,
        filename,
        mime,
      };
      put(
        record,
        h(
          "div",
          { class: "bc-card" },
          h(
            "div",
            { class: "bc-card__row" },
            h("span", { class: "bc-option-icon" }, icon(ICON_FILE)),
            h(
              "span",
              {
                style:
                  "flex:1 1 auto;min-width:0;display:flex;flex-direction:column",
              },
              h(
                "span",
                { class: "bc-option-title", style: "overflow-wrap:anywhere" },
                filename || "—",
              ),
              meta,
            ),
            slot,
          ),
        ),
      );
      const caption = str(value.caption);
      if (caption) put(record, h("p", { class: "bc-reply" }, clip(caption)));
      return describe(record.media, t.receiving);
    }
    const item = agents.get(id)?.media;
    if (!item) return;
    if (phase === "delta") {
      const binary = atob(str(value.data));
      const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
      item.size += bytes.length;
      if (item.kept && item.size <= MAX_MEDIA) item.chunks.push(bytes);
      else {
        item.kept = false;
        item.chunks = [];
      }
      return describe(item, t.receiving);
    }
    if (phase === "end") {
      const error = str(value.error);
      if (error) {
        item.chunks = [];
        return describe(item, `${t.mediaFailed} · ${error}`);
      }
      describe(item, t.received);
      if (!item.kept || !item.size) return;
      const url = URL.createObjectURL(
        new Blob(item.chunks as BlobPart[], {
          type: item.mime || "application/octet-stream",
        }),
      );
      item.chunks = [];
      urls.add(url);
      item.slot.replaceChildren(
        h(
          "a",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            href: url,
            download: item.filename || "attachment",
          },
          icon(ICON_DOWNLOAD),
          t.download,
        ),
      );
    }
  }

  /** 「`application/gpx+xml · 18 KB` · 已接收」: the machine values mono, the state in words. */
  function describe(item: Media, status: string) {
    const size =
      item.size < 1024
        ? `${item.size} B`
        : item.size < 1024 * 1024
          ? `${Math.round(item.size / 1024)} KB`
          : `${(item.size / 1024 / 1024).toFixed(1)} MB`;
    item.meta.replaceChildren(
      h(
        "span",
        { class: "bc-mono" },
        [item.mime, size].filter(Boolean).join(" · "),
      ),
      ` · ${status}`,
    );
  }

  // ---- turn state: the spinning marks, 「思考中」, the send button, and when the queue moves
  function busy() {
    return active.size > 0 || awaiting.size > 0;
  }

  function refresh() {
    const working = busy() || typingOn;
    // something visibly moving already speaks for the device: streaming text, a running tool,
    // or a question waiting on the reader
    const moving =
      !!request || [...active].some((record) => record.run || record.tool);
    for (const each of groups) {
      if (!each.node.isConnected) {
        spin(each, false);
        groups.delete(each);
        continue;
      }
      spin(
        each,
        [...active].some((record) => record.group === each),
      );
    }
    // 「思考中」 follows the running turn, or a turn of its own while the device has not begun
    thinking.remove();
    pending.node.remove();
    const last = group?.node.isConnected ? group : null;
    const running =
      !!last && [...active].some((record) => record.group === last);
    let host: Group | null = null;
    if (working && !moving) {
      if (running && column.lastElementChild === last!.node) host = last;
      else if (!running) {
        host = pending;
        column.append(pending.node);
      }
    }
    dots.remove();
    if (host) {
      if (host === pending) spin(pending, true);
      // without a spinning mark, the dots show the device is at work
      if (!host.spin) thinking.prepend(dots);
      host.node.append(thinking);
    }
    if (host !== pending) spin(pending, false);
    const queueing = busy() && !request;
    const label = queueing ? t.enqueue : t.send;
    if (sendButton.button.getAttribute("aria-label") !== label)
      sendButton.set(label);
    sendButton.button.disabled = !canSend();
    pump();
  }

  function canSend() {
    return online() && !!input.value.trim() && !over();
  }

  /** Sends the oldest queued message once the device is free. */
  function pump() {
    if (!queue.length || busy() || !online()) return;
    const next = queue.shift()!;
    if (!send(next.text, next.replyTo)) queue.unshift(next);
    renderQueue();
  }

  /** Stops waiting on sent messages once nothing has been heard for `STALL_MS`. */
  function arm() {
    clearTimeout(stall);
    stall = awaiting.size
      ? setTimeout(() => {
          awaiting.clear();
          refresh();
        }, STALL_MS)
      : undefined;
  }

  function renderQueue() {
    const bars = queue.map((item, index) =>
      h(
        "div",
        {
          class: `bc-composer__bar${index === queue.length - 1 ? " bc-composer__bar--rule" : ""}`,
          role: "listitem",
        },
        h(
          "span",
          { class: "bc-mono bc-muted", style: "flex:none" },
          pad(index + 1),
        ),
        h("span", { class: "bc-composer__text" }, item.text),
        iconButton(t.editQueued, ICON_PENCIL, () => {
          const [taken] = queue.splice(queue.indexOf(item), 1);
          input.value = input.value
            ? `${taken.text}\n${input.value}`
            : taken.text;
          if (taken.replyTo) setReply(taken.replyTo);
          renderQueue();
          changed();
          input.focus();
        }).button,
        iconButton(t.removeQueued, ICON_X, () => {
          queue.splice(queue.indexOf(item), 1);
          renderQueue();
        }).button,
      ),
    );
    queueList.replaceChildren(
      ...(queue.length
        ? [
            h(
              "div",
              { class: "bc-composer__bar" },
              icon(ICON_CLOCK, undefined, "bc-muted"),
              h(
                "span",
                { class: "bc-muted" },
                `${t.queued} · `,
                h("span", { class: "bc-mono" }, pad(queue.length)),
              ),
            ),
            ...bars,
          ]
        : []),
    );
    show(queueList, queue.length > 0);
  }

  // ---- composer
  const encoder = new TextEncoder();
  // where the browser cannot size the textarea to its text, the page does
  const fits = !!globalThis.CSS?.supports?.("field-sizing", "content");
  function over() {
    return encoder.encode(input.value).byteLength > MAX_BYTES;
  }
  function changed() {
    // past the limit nothing counts: send turns grey and one sentence says why
    showInvalid(over() ? t.tooLong : "");
    if (!fits) {
      input.style.height = "auto";
      if (input.scrollHeight)
        input.style.height = `${Math.min(input.scrollHeight, MAX_INPUT)}px`;
    }
    sendButton.button.disabled = !canSend();
  }
  function showInvalid(message: string) {
    invalid.textContent = message;
    show(invalid, !!message);
    if (message) {
      input.setAttribute("aria-invalid", "true");
      input.setAttribute("aria-describedby", invalid.id);
    } else {
      input.removeAttribute("aria-invalid");
      input.removeAttribute("aria-describedby");
    }
  }

  function answer(text: string) {
    send(text, null);
  }

  function submit() {
    const text = input.value;
    if (!text.trim() || over()) return input.focus();
    // a pending question takes the message as its answer at once; otherwise a running turn
    // queues it
    if (!request && (busy() || queue.length)) {
      queue.push({ text, replyTo });
      renderQueue();
    } else if (!send(text, replyTo)) return;
    input.value = "";
    setReply(null);
    changed();
    refresh();
  }

  function send(text: string, target: { id: string; text: string } | null) {
    const connection = socket;
    if (!connection || connection.readyState !== WebSocket.OPEN) return false;
    if (connection.bufferedAmount > 4096) {
      context.toast({ kind: "error", title: t.notSent, body: t.busy });
      return false;
    }
    try {
      connection.send(
        JSON.stringify({ text: target ? withQuote(text, target.text) : text }),
      );
    } catch {
      context.toast({ kind: "error", title: t.notSent });
      return false;
    }
    // the bridge numbers each accepted frame on this connection: web-in-1, web-in-2, …
    const id = `web-in-${++inbound}`;
    const entry = addUser(text, id, target ? target.text : null);
    if (request) {
      // an answer goes on the asking turn; no turn of its own starts
      answered = { owner: request.owner, entry };
      setRequest(null);
    } else {
      awaiting.add(id);
      arm();
    }
    stuck = true;
    follow();
    refresh();
    return true;
  }

  input.addEventListener("input", changed);
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      submit();
    }
  });

  // ---- scrolling: follow the conversation while the reader is at its foot
  let stuck = true;

  function nearBottom() {
    const scroller = doc.scrollingElement;
    return (
      !scroller ||
      scroller.scrollHeight - scroller.scrollTop - window.innerHeight < 120
    );
  }
  function follow() {
    const scroller = doc.scrollingElement;
    if (scroller && stuck) scroller.scrollTop = scroller.scrollHeight;
    show(jump, !stuck && items.length > 0);
  }
  window.addEventListener(
    "scroll",
    () => {
      stuck = nearBottom();
      show(jump, !stuck && items.length > 0);
    },
    { signal, passive: true },
  );
  // a hidden tab pauses frames: what is still flowing lands at once
  doc.addEventListener(
    "visibilitychange",
    () => {
      for (const run of [...flowing]) settle(run);
    },
    { signal },
  );

  // ---- connection
  let socket: WebSocket | undefined;
  let inbound = 0;
  let warned = false;

  function online() {
    return socket?.readyState === WebSocket.OPEN;
  }

  // the top bar carries the connection state, as the design's chat boards show it
  function setLink(state: "connecting" | "live" | "lost") {
    context.badge?.(
      state === "connecting"
        ? null
        : { label: state === "live" ? t.live : t.lost, tone: state },
    );
  }

  function connect(byUser: boolean) {
    socket?.close();
    socket = undefined;
    input.disabled = false;
    show(offline, false);
    sent = new Map();
    inbound = 0;
    warned = false;
    refresh();
    const url = new URL("/ws/message", document.baseURI);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    let connection: WebSocket;
    try {
      connection = new WebSocket(url);
    } catch {
      return disconnected();
    }
    socket = connection;
    const live = () => socket === connection && !signal.aborted;
    connection.addEventListener("open", () => {
      if (!live()) return;
      setLink("live");
      if (byUser) context.toast({ kind: "success", title: t.reconnected });
      refresh();
    });
    connection.addEventListener("message", (event) => {
      if (!live()) return;
      if (stuck) stuck = nearBottom();
      try {
        receive(event.data);
      } catch {
        if (!warned) context.toast({ kind: "error", title: t.unreadable });
        warned = true;
      }
      // anything heard means the device is alive: keep waiting on its queue
      if (awaiting.size) arm();
      refresh();
      follow();
    });
    const lost = () => {
      if (live()) disconnected();
    };
    connection.addEventListener("error", lost);
    connection.addEventListener("close", lost);
  }

  function disconnected() {
    socket = undefined;
    setLink("lost");
    input.disabled = true;
    typingOn = false;
    caret.remove();
    for (const record of agents.values()) close(record);
    // live events only: a turn cut off here is not heard ending, so the page stops waiting on it;
    // queued messages stay and go out after reconnecting
    active.clear();
    awaiting.clear();
    clearTimeout(stall);
    const count = [...sent.values()].filter((entry) => !entry.confirmed).length;
    // what to do with them is the reader's action, so it is said, not hidden behind a term
    offlineCount.replaceChildren(
      count
        ? h(
            "span",
            null,
            " · ",
            h("span", { class: "bc-mono" }, String(count)),
            t.unconfirmed(count),
          )
        : "",
    );
    show(offline, true);
    refresh();
  }

  signal.addEventListener(
    "abort",
    () => {
      const connection = socket;
      socket = undefined;
      connection?.close();
      cancelAnimationFrame(frame);
      clearTimeout(stall);
      flowing.clear();
      for (const each of [...groups, pending]) each.spin?.stop();
      for (const url of urls) URL.revokeObjectURL(url);
      urls.clear();
      agents.clear();
      sent.clear();
      queue.length = 0;
      input.value = "";
      style.remove();
    },
    { once: true },
  );

  column.append(empty);
  setFresh(true);
  changed();
  connect(false);
  return page;
});
