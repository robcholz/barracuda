import {
  ICON_ARROW_UP,
  ICON_CHECK,
  ICON_CHEVRON_RIGHT,
  ICON_CIRCLE_ALERT,
  ICON_CIRCLE_X,
  ICON_DOWNLOAD,
  ICON_FILE,
  ICON_LIGHTBULB,
  ICON_MESSAGE_DASHED,
  ICON_REPLY,
  ICON_ROTATE_CW,
  ICON_SHIELD_ALERT,
  ICON_WIFI_OFF,
  ICON_WRENCH,
  ICON_X,
  MARK_TILE,
  definePage,
  h,
  icon,
  mark,
  term,
  type PortalContext,
} from "../../../captive-portal/resources/web/ui";

/**
 * Web chat over the bridge's WebSocket `/ws/message`. The page sends `WebClientFrame` JSON
 * (`{ text, reply_to? }`) and renders the SSE-formatted frames the bridge pushes: message
 * lifecycle (`message.start`/`delta`/`end`/`edit`/`delete`/`reaction`), the Agent's semantic
 * events inside `message.event`, attachments, typing and `stream.lagged`. Live events only.
 */

const STRINGS = {
  zh: {
    log: "聊天记录",
    sent: "已发送",
    sentTip: "设备不回执送达或已读",
    reasoning: "思考过程",
    ok: "成功",
    failed: "失败",
    args: "参数",
    output: "输出",
    step: "第 {n} 步",
    usage: "输入 {i} · 输出 {o} tokens",
    reply: "回复",
    edited: "已编辑",
    typing: "正在输入",
    approvalTitle: "需要你的允许",
    approvalTool: "工具",
    approvalArgs: "参数",
    approvalReason: "理由",
    allow: "允许",
    deny: "拒绝",
    approvalHint: "或直接回复",
    answering: "正在答复",
    answerPh: "回复这次请求…",
    emptyTitle: "还没有消息",
    emptyBody: "发一条消息，或从这里开始：",
    suggestions: [
      "今天适合骑车吗？",
      "总结一下我今天的日程",
      "提醒我 6 点下班",
    ],
    incomplete: "未完成 · 事件流中断",
    lagged: "错过 {n} 个事件",
    turnError: "这一轮出错",
    offline: "连接已中断",
    unconfirmed: "{n} 条未确认",
    unconfirmedTip: "不会自动重发",
    reconnect: "重新连接",
    reconnected: "已重新连接",
    label: "消息",
    placeholder: "给设备发一条消息",
    send: "发送",
    replying: "回复 Barracuda",
    cancelReply: "取消回复",
    session: "临时会话",
    sessionTip: "只显示连接后的消息，刷新页面后清空",
    download: "下载",
    receiving: "接收中",
    received: "已接收",
    mediaFailed: "接收失败",
    reaction: "回应",
    tooLong: "消息超过 1024 字节，请缩短后发送。",
    notSent: "消息未发出",
    busy: "发送队列繁忙，请稍后再试。",
    unreadable: "收到无法识别的消息",
  },
  en: {
    log: "Conversation",
    sent: "Sent",
    sentTip: "The device sends no delivery or read receipt",
    reasoning: "Reasoning",
    ok: "Succeeded",
    failed: "Failed",
    args: "Arguments",
    output: "Output",
    step: "Step {n}",
    usage: "{i} in · {o} out tokens",
    reply: "Reply",
    edited: "Edited",
    typing: "Typing",
    approvalTitle: "Needs your permission",
    approvalTool: "Tool",
    approvalArgs: "Arguments",
    approvalReason: "Reason",
    allow: "Allow",
    deny: "Deny",
    approvalHint: "Or reply in your own words",
    answering: "Answering",
    answerPh: "Reply to this request…",
    emptyTitle: "No messages yet",
    emptyBody: "Send a message, or start here:",
    suggestions: [
      "Is it a good day for a ride?",
      "Summarize today's schedule",
      "Remind me to leave at 6",
    ],
    incomplete: "Incomplete · event stream cut off",
    lagged: "Missed {n} events",
    turnError: "This turn failed",
    offline: "Disconnected",
    unconfirmed: "{n} unconfirmed",
    unconfirmedTip: "Not resent automatically",
    reconnect: "Reconnect",
    reconnected: "Reconnected",
    label: "Message",
    placeholder: "Message the device",
    send: "Send",
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
    tooLong: "Messages are limited to 1024 bytes; shorten it to send.",
    notSent: "Message not sent",
    busy: "The send queue is full; try again shortly.",
    unreadable: "Received a message this page can't read",
  },
};

/** The bridge's limits as the previous page enforced them. */
const MAX_BYTES = 1024;
const MAX_ITEMS = 100;
const MAX_TEXT = 65536;
const MAX_FRAME = 262144;
const MAX_MEDIA = 8 * 1024 * 1024;

const ROW = "display:flex;flex-direction:column;gap:10px;max-width:86%";
const MONO12 = "font-size:12px";
const numbers = new Intl.NumberFormat("en-US");
const fill = (text: string, map: Record<string, string | number>) =>
  text.replace(/\{(\w+)\}/g, (_, key: string) => String(map[key]));
const clip = (text: string) =>
  text.length > MAX_TEXT ? text.slice(-MAX_TEXT) : text;

type Data = Record<string, unknown>;
const str = (value: unknown) => (typeof value === "string" ? value : "");

/** Shows or hides a node, whatever display its inline style or class gives it. */
function show(node: HTMLElement, on: boolean) {
  node.hidden = !on;
  if (on) node.style.display = node.dataset.display ?? "";
  else {
    if (node.style.display !== "none")
      node.dataset.display = node.style.display;
    node.style.display = "none";
  }
}

/** One fold (reasoning or a tool card): a button that shows and hides `panel`. */
function fold(button: HTMLButtonElement, panel: HTMLElement, open: boolean) {
  const chevron = button.lastElementChild as SVGElement;
  const set = (value: boolean) => {
    button.setAttribute("aria-expanded", String(value));
    show(panel, value);
    chevron.style.transform = value ? "rotate(90deg)" : "none";
  };
  button.addEventListener("click", () =>
    set(button.getAttribute("aria-expanded") !== "true"),
  );
  set(open);
  return set;
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

interface Agent {
  id: string;
  kind: string;
  root: HTMLElement;
  /** The open text run and which stream it belongs to. */
  run: { stream: string; text: Text; node: HTMLElement } | null;
  tool: Tool | null;
  request: Request | null;
  steps: number;
  usage: { i?: number; o?: number };
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

export const mount = definePage((context: PortalContext) => {
  const { lang, signal } = context;
  const t = STRINGS[lang];
  const doc = document;

  // ---- layout
  const column = h("div", {
    style:
      "flex:1 1 auto;width:100%;max-width:760px;margin:0 auto;display:flex;flex-direction:column;gap:var(--space-6)",
  });
  const log = h(
    "div",
    {
      class: "bc-page",
      role: "log",
      "aria-label": t.log,
      "aria-live": "polite",
      style: "flex:1 1 auto;max-width:none",
    },
    column,
  );
  const empty = h(
    "div",
    {
      style:
        "flex:1 1 auto;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:12px;padding:96px 0;text-align:center",
    },
    h(
      "span",
      { class: "bc-muted", style: "display:inline-flex" },
      icon(ICON_MESSAGE_DASHED, 32),
    ),
    h("h2", { class: "bc-title" }, t.emptyTitle),
    h(
      "p",
      { class: "bc-small bc-muted", style: "margin:0;max-width:360px" },
      t.emptyBody,
    ),
    h(
      "div",
      {
        style:
          "display:flex;flex-wrap:wrap;justify-content:center;gap:8px;margin-top:8px",
      },
      t.suggestions.map((text) =>
        h(
          "button",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            type: "button",
            style: "font-weight:400",
            onclick: () => {
              input.value = text;
              changed();
              submit();
            },
          },
          text,
        ),
      ),
    ),
  );
  const typing = h(
    "span",
    {
      class: "bc-small bc-muted",
      style: "display:flex;align-items:center;gap:8px",
    },
    h(
      "span",
      { style: "display:inline-flex;gap:3px", "aria-hidden": "true" },
      [0, 1, 2].map(() =>
        h("span", {
          style:
            "width:4px;height:4px;border-radius:2px;background:var(--muted-foreground)",
        }),
      ),
    ),
    t.typing,
  );
  show(typing, false);
  const caret = h("span", {
    "aria-hidden": "true",
    style:
      "display:inline-block;width:7px;height:15px;margin-left:2px;vertical-align:-2px;background:var(--foreground)",
  });

  const input = h("textarea", {
    id: "imessage-web-message",
    rows: 3,
    placeholder: t.placeholder,
  });
  const counter = h("span", { class: "bc-mono" });
  const sendButton = h(
    "button",
    {
      class: "bc-button bc-button--icon",
      type: "submit",
      "aria-label": t.send,
      disabled: true,
    },
    icon(ICON_ARROW_UP),
  );
  const answering = h(
    "div",
    {
      style:
        "display:flex;align-items:center;gap:8px;padding:8px 12px;border-bottom:1px solid var(--border);background:var(--amber-100);border-radius:7px 7px 0 0;color:var(--amber-1000);font-size:12px",
    },
    h("span", { style: "display:inline-flex" }, icon(ICON_SHIELD_ALERT, 14)),
  );
  const answeringText = h("span");
  answering.append(answeringText);
  show(answering, false);
  const quote = h("span", {
    style:
      "flex:1 1 auto;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis",
  });
  const replyBar = h(
    "div",
    {
      style:
        "display:flex;align-items:center;gap:8px;padding:8px 8px 0 14px;font-size:12px",
    },
    h(
      "span",
      { class: "bc-muted", style: "display:inline-flex" },
      icon(ICON_REPLY, 12),
    ),
    h("span", { class: "bc-muted", style: "flex:none" }, t.replying),
    quote,
    h(
      "button",
      {
        class: "bc-toast__close",
        type: "button",
        "aria-label": t.cancelReply,
        onclick: () => setReply(null),
      },
      icon(ICON_X, 14),
    ),
  );
  show(replyBar, false);
  const composer = h(
    "div",
    { class: "bc-composer" },
    answering,
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
    input,
    h(
      "div",
      {
        style:
          "display:flex;align-items:center;justify-content:space-between;gap:12px;padding:8px 8px 8px 14px",
      },
      h(
        "span",
        { class: "bc-muted", style: MONO12 },
        term(t.session, t.sessionTip, lang, { start: true }),
        counter,
      ),
      sendButton,
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
    {
      class: "bc-alert bc-alert--error",
      role: "alert",
      style: "align-items:center;padding:10px 12px",
    },
    h(
      "span",
      { style: "display:inline-flex;color:var(--destructive)" },
      icon(ICON_WIFI_OFF),
    ),
    h(
      "span",
      { class: "bc-small", style: "flex:1 1 auto" },
      h("span", { style: "font-weight:500" }, t.offline),
      offlineCount,
    ),
    h(
      "button",
      {
        class: "bc-button bc-button--outline bc-button--sm",
        type: "button",
        onclick: () => connect(true),
      },
      icon(ICON_ROTATE_CW, 14),
      t.reconnect,
    ),
  );
  show(offline, false);
  const form = h(
    "form",
    {
      class: "bc-page",
      style:
        "position:sticky;bottom:0;max-width:none;padding-top:0;background:var(--background)",
      onsubmit: (event: Event) => {
        event.preventDefault();
        submit();
      },
    },
    h(
      "div",
      {
        style:
          "width:100%;max-width:760px;margin:0 auto;display:flex;flex-direction:column;gap:8px",
      },
      offline,
      composer,
      invalid,
    ),
  );

  // ---- conversation state
  const items: { node: HTMLElement; forget(): void }[] = [];
  const agents = new Map<string, Agent>();
  let sent = new Map<string, Sent>();
  const urls = new Set<string>();
  let group: HTMLElement | null = null;
  let request: Request | null = null;
  /** The message that answered a permission request, until the asking turn goes on. */
  let answered: { owner: Agent; entry: Sent } | null = null;
  let replyTo: { id: string; text: string } | null = null;

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
    empty.remove();
    column.append(typing);
  }

  function who() {
    return h(
      "span",
      {
        class: "bc-muted",
        style:
          "display:flex;align-items:center;gap:6px;font-size:12px;font-weight:500",
      },
      h(
        "span",
        { style: "display:inline-flex;color:var(--foreground)" },
        mark(MARK_TILE, 16),
      ),
      "Barracuda",
    );
  }

  function reactionBadge() {
    const badge = h("span", {
      class: "bc-badge",
      style: "height:20px;padding:0 6px;font-size:12px",
      "aria-label": t.reaction,
    });
    show(badge, false);
    return badge;
  }

  function addUser(text: string, id: string, quoted: string | null) {
    const reaction = reactionBadge();
    const node = h(
      "div",
      {
        style:
          "align-self:flex-end;max-width:78%;display:flex;flex-direction:column;align-items:flex-end;gap:6px",
      },
      quoted === null
        ? null
        : h(
            "span",
            {
              class: "bc-small bc-muted",
              style:
                "display:flex;align-items:center;gap:6px;max-width:100%;padding-left:8px;border-left:2px solid var(--border)",
            },
            icon(ICON_REPLY, 12),
            h(
              "span",
              {
                style:
                  "white-space:nowrap;overflow:hidden;text-overflow:ellipsis",
              },
              quoted,
            ),
          ),
      h("p", { class: "bc-bubble" }, text),
      h(
        "span",
        {
          class: "bc-muted",
          style: "display:flex;align-items:center;gap:8px;font-size:12px",
        },
        reaction,
        term(t.sent, t.sentTip, lang),
      ),
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
    if (!group) {
      group = h("div", { style: ROW }, who());
      column.append(group);
    }
    const root = h("div", {
      style: "display:flex;flex-direction:column;gap:10px",
    });
    const record: Agent = {
      id,
      kind,
      root,
      run: null,
      tool: null,
      request: null,
      steps: 0,
      usage: {},
      footer: null,
      reaction: reactionBadge(),
      ended: false,
    };
    agents.set(id, record);
    const owner = group;
    add(root, () => forget(record), owner);
    group = owner;
    return record;
  }

  function forget(record: Agent) {
    agents.delete(record.id);
    if (request?.owner === record) setRequest(null);
    if (replyTo?.id === record.id) setReply(null);
  }

  /** Appends text to the message's run for `stream`, opening a new run when the stream changes. */
  function write(record: Agent, stream: string, text: string) {
    if (!text) return;
    if (record.run?.stream !== stream) {
      const node =
        stream === "reasoning"
          ? reasoning(record)
          : h("p", {
              class:
                stream === "notice" || stream === "tool"
                  ? "bc-small bc-muted"
                  : "bc-reply",
              style:
                stream === "notice" || stream === "tool" ? "margin:0" : null,
            });
      if (stream === "tool") node.append(icon(ICON_WRENCH, 12), " ");
      const content = doc.createTextNode("");
      node.append(content);
      if (stream !== "reasoning") put(record, node);
      record.run = { stream, text: content, node };
    }
    const run = record.run!;
    run.text.data = clip(run.text.data + text);
    if (stream === "output" && !record.ended) run.node.append(caret);
  }

  /** Adds a part to a message, above its footer. */
  function put(record: Agent, node: HTMLElement) {
    record.root.insertBefore(
      node,
      record.footer?.parentElement ? record.footer : null,
    );
  }

  function reasoning(record: Agent) {
    const panel = h("p", {
      class: "bc-small bc-muted",
      style:
        "margin:6px 0 0;padding-left:12px;border-left:2px solid var(--border);white-space:pre-wrap;overflow-wrap:anywhere",
    });
    const button = h(
      "button",
      { class: "bc-fold-link", type: "button" },
      icon(ICON_LIGHTBULB, 14),
      t.reasoning,
      icon(ICON_CHEVRON_RIGHT, 14),
    );
    fold(button, panel, false);
    put(record, h("div", null, button, panel));
    return panel;
  }

  function tool(record: Agent): Tool {
    const name = h("span", {
      class: "bc-mono",
      style: "font-size:13px;font-weight:500",
    });
    const summary = h("span", {
      class: "bc-mono bc-muted",
      style:
        "flex:1 1 auto;min-width:0;font-size:12px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis",
    });
    const status = h("span", {
      class: "bc-small",
      style:
        "display:inline-flex;align-items:center;gap:4px;flex:none;white-space:nowrap",
    });
    const button = h(
      "button",
      { class: "bc-fold", type: "button" },
      h(
        "span",
        { class: "bc-muted", style: "display:inline-flex" },
        icon(ICON_WRENCH, 14),
      ),
      name,
      summary,
      status,
      h(
        "span",
        { class: "bc-muted", style: "display:inline-flex" },
        icon(ICON_CHEVRON_RIGHT, 14),
      ),
    );
    const code = "white-space:pre-wrap;overflow-wrap:anywhere;font-size:12px";
    const args = h("code", { class: "bc-mono", style: code });
    const out = h("code", { class: "bc-mono", style: code });
    const panel = h(
      "div",
      {
        style:
          "display:grid;grid-template-columns:72px minmax(0,1fr);gap:6px 12px;padding:10px 12px 12px;border-top:1px solid var(--border);font-size:12px;line-height:18px",
      },
      h("span", { class: "bc-muted" }, t.args),
      args,
      h("span", { class: "bc-muted" }, t.output),
      out,
    );
    const open = fold(button, panel, false);
    const card = h("div", { class: "bc-card" }, button, panel);
    put(record, card);
    record.run = null;
    return { card, name, summary, args, out, status, open };
  }

  function approval(record: Agent): Request {
    const tool = h("dd", {
      class: "bc-mono",
      style: "margin:0;font-weight:500",
    });
    const args = h("code", { class: "bc-code" });
    const reason = h("dd", { style: "margin:0;overflow-wrap:anywhere" });
    const actions = h(
      "div",
      {
        class: "bc-card__foot",
        style:
          "display:flex;flex-wrap:wrap;align-items:center;gap:8px;padding:12px 14px",
      },
      h(
        "button",
        {
          class: "bc-button bc-button--sm",
          type: "button",
          onclick: () => answer(t.allow),
        },
        icon(ICON_CHECK, 14),
        t.allow,
      ),
      h(
        "button",
        {
          class: "bc-button bc-button--outline bc-button--sm",
          type: "button",
          onclick: () => answer(t.deny),
        },
        icon(ICON_X, 14),
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
      {
        class: "bc-card",
        "aria-labelledby": title,
        style: "border-color:var(--amber-400)",
      },
      h(
        "div",
        {
          style:
            "display:flex;align-items:center;gap:10px;padding:12px 14px;border-bottom:1px solid var(--border)",
        },
        h(
          "span",
          { style: "display:inline-flex;color:var(--warning)" },
          icon(ICON_SHIELD_ALERT, 18),
        ),
        h(
          "h3",
          { id: title, class: "bc-title", style: "font-size:14px;margin:0" },
          t.approvalTitle,
        ),
      ),
      h(
        "dl",
        {
          style:
            "display:grid;grid-template-columns:72px minmax(0,1fr);gap:8px 12px;margin:0;padding:12px 14px;font-size:13px",
        },
        h("dt", { class: "bc-muted" }, t.approvalTool),
        tool,
        h("dt", { class: "bc-muted" }, t.approvalArgs),
        h("dd", { style: "margin:0;min-width:0" }, args),
        h("dt", { class: "bc-muted" }, t.approvalReason),
        reason,
      ),
      actions,
    );
    show(actions, false);
    record.root.parentElement!.style.width = "100%";
    put(record, card);
    record.run = null;
    return { card, tool, args, reason, actions, owner: record };
  }

  /** Shows the message's footer (steps, usage, reaction, Reply): at its end or while it asks. */
  function showFooter(record: Agent) {
    if (record.kind !== "reply") return;
    if (!record.footer) {
      const facts = h("span", { class: "bc-mono bc-muted" });
      record.footer = h(
        "div",
        {
          style:
            "display:flex;align-items:center;gap:4px 12px;flex-wrap:wrap;font-size:12px",
        },
        facts,
        record.reaction,
        h(
          "button",
          {
            class: "bc-button bc-button--ghost bc-button--sm",
            type: "button",
            style:
              "height:24px;padding:0 6px;font-size:12px;font-weight:400;color:var(--muted-foreground)",
            onclick: () => setReply({ id: record.id, text: textOf(record) }),
          },
          icon(ICON_REPLY, 12),
          t.reply,
        ),
      );
    }
    record.root.append(record.footer);
    footer(record);
  }

  /** Refreshes the footer's facts, if it is shown. */
  function footer(record: Agent) {
    if (!record.footer) return;
    const parts: string[] = [];
    if (record.steps) parts.push(fill(t.step, { n: record.steps }));
    const { i, o } = record.usage;
    if (i !== undefined || o !== undefined)
      parts.push(
        fill(t.usage, {
          i: i === undefined ? "—" : numbers.format(i),
          o: o === undefined ? "—" : numbers.format(o),
        }),
      );
    const facts = record.footer.firstElementChild as HTMLElement;
    facts.textContent = parts.join(" · ");
    show(facts, parts.length > 0);
  }

  function textOf(record: Agent) {
    return [...record.root.querySelectorAll("p.bc-reply")]
      .map((node) => node.firstChild?.textContent ?? "")
      .join("\n")
      .trim()
      .split("\n")[0];
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
    answeringText.textContent = next
      ? `${t.answering} · ${next.tool.textContent}`
      : "";
    input.placeholder = next ? t.answerPh : t.placeholder;
  }

  function alert(title: string, text: string) {
    add(
      h(
        "div",
        { class: "bc-alert bc-alert--error", role: "alert" },
        h(
          "span",
          {
            style:
              "display:inline-flex;color:var(--destructive);margin-top:2px",
          },
          icon(ICON_CIRCLE_ALERT),
        ),
        h(
          "span",
          { style: "display:flex;flex-direction:column;gap:2px" },
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
    const line = () =>
      h("span", { style: "flex:1 1 auto;border-top:1px dashed var(--input)" });
    add(
      h(
        "div",
        {
          role: "separator",
          class: "bc-mono bc-muted",
          style: "display:flex;align-items:center;gap:12px;font-size:12px",
        },
        line(),
        text,
        line(),
      ),
    );
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
      case "iteration_started":
        record.steps += 1;
        return footer(record);
      case "reasoning_delta":
        return write(record, "reasoning", text);
      case "output_delta":
      case "effect_output_delta":
        return write(record, "output", text);
      case "reasoning_ended":
      case "output_ended":
      case "effect_output_ended":
        caret.remove();
        record.run = null;
        return;
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
        card.status.style.color = ok ? "var(--success)" : "var(--destructive)";
        card.status.replaceChildren(
          icon(ok ? ICON_CHECK : ICON_CIRCLE_X, 12),
          ok ? t.ok : t.failed,
        );
        if (!ok) card.out.style.color = "var(--destructive)";
        card.open(!ok);
        record.tool = null;
        return;
      }
      case "usage":
        for (const [key, name] of [
          ["i", "input_tokens"],
          ["o", "output_tokens"],
        ] as const) {
          const value = payload[name];
          if (typeof value === "number")
            record.usage[key] = (record.usage[key] ?? 0) + value;
        }
        return footer(record);
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
        return alert(t.turnError, str(payload.message));
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
      show(typing, value.typing === true);
      return column.append(typing);
    }
    const id = str(value.message_id);
    if (!id) return;
    const known = agents.get(id);
    switch (event) {
      case "message.start": {
        agent(id, str(value.kind) || "reply");
        const target = sent.get(str(value.reply_to));
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
        known.run = null;
        caret.remove();
        if (request?.owner === known) setRequest(null);
        const error = str(value.error);
        if (error)
          put(
            known,
            h(
              "span",
              {
                class: "bc-small",
                style:
                  "display:flex;flex-wrap:wrap;align-items:center;gap:6px;color:var(--destructive)",
              },
              icon(ICON_CIRCLE_ALERT, 14),
              t.incomplete,
              h("span", { class: "bc-mono", style: MONO12 }, error),
            ),
          );
        return showFooter(known);
      }
      case "message.edit": {
        if (!known) return;
        for (const node of known.root.querySelectorAll("p.bc-reply"))
          node.remove();
        known.run = null;
        write(known, "output", str(value.text) || " ");
        caret.remove();
        known.run!.node.append(
          h("span", { class: "bc-muted", style: MONO12 }, ` · ${t.edited}`),
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
          if (parent === group) group = null;
          parent.remove();
        }
        if (!items.length) column.prepend(empty);
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
      const meta = h("span", { class: "bc-mono bc-muted", style: MONO12 });
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
          {
            class: "bc-card",
            style: "display:flex;align-items:center;gap:12px;padding:10px 12px",
          },
          h(
            "span",
            {
              style:
                "flex:none;display:flex;align-items:center;justify-content:center;width:36px;height:36px;border-radius:var(--radius-sm);background:var(--muted)",
            },
            icon(ICON_FILE, 18),
          ),
          h(
            "span",
            {
              style:
                "flex:1 1 auto;min-width:0;display:flex;flex-direction:column",
            },
            h(
              "span",
              { style: "font-weight:500;overflow-wrap:anywhere" },
              filename || "—",
            ),
            meta,
          ),
          slot,
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
          icon(ICON_DOWNLOAD, 14),
          t.download,
        ),
      );
    }
  }

  function describe(item: Media, status: string) {
    const size =
      item.size < 1024
        ? `${item.size} B`
        : item.size < 1024 * 1024
          ? `${Math.round(item.size / 1024)} KB`
          : `${(item.size / 1024 / 1024).toFixed(1)} MB`;
    item.meta.textContent = [item.mime, size, status]
      .filter(Boolean)
      .join(" · ");
  }

  // ---- composer
  const encoder = new TextEncoder();
  function changed() {
    const bytes = encoder.encode(input.value).byteLength;
    counter.textContent = ` · ${bytes} / ${MAX_BYTES} bytes`;
    const over = bytes > MAX_BYTES;
    counter.style.color = over ? "var(--destructive)" : "";
    if (!over) showInvalid("");
  }
  function showInvalid(message: string) {
    invalid.textContent = message;
    show(invalid, !!message);
    composer.style.borderColor = message ? "var(--destructive)" : "";
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
    if (!text.trim()) return input.focus();
    if (encoder.encode(text).byteLength > MAX_BYTES) {
      showInvalid(t.tooLong);
      return input.focus();
    }
    if (send(text, replyTo)) {
      input.value = "";
      setReply(null);
      changed();
    }
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
        JSON.stringify(target ? { text, reply_to: target.id } : { text }),
      );
    } catch {
      context.toast({ kind: "error", title: t.notSent });
      return false;
    }
    // the bridge numbers each accepted frame on this connection: web-in-1, web-in-2, …
    const entry = addUser(
      text,
      `web-in-${++inbound}`,
      target ? target.text : null,
    );
    if (request) {
      answered = { owner: request.owner, entry };
      setRequest(null);
    }
    scroll(true);
    return true;
  }

  input.addEventListener("input", changed);
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      submit();
    }
  });

  // ---- connection
  let socket: WebSocket | undefined;
  let inbound = 0;
  let warned = false;

  function setOnline(online: boolean) {
    sendButton.disabled = !online;
  }

  function nearBottom() {
    const page = doc.scrollingElement;
    return (
      !page || page.scrollHeight - page.scrollTop - window.innerHeight < 120
    );
  }
  function scroll(force: boolean) {
    const page = doc.scrollingElement;
    if (page && force) page.scrollTop = page.scrollHeight;
  }

  function connect(byUser: boolean) {
    socket?.close();
    socket = undefined;
    setOnline(false);
    input.disabled = false;
    show(offline, false);
    sent = new Map();
    inbound = 0;
    warned = false;
    const url = new URL("/ws/message", document.baseURI);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    let connection: WebSocket;
    try {
      connection = new WebSocket(url);
    } catch {
      return disconnected();
    }
    socket = connection;
    const active = () => socket === connection && !signal.aborted;
    connection.addEventListener("open", () => {
      if (!active()) return;
      setOnline(true);
      if (byUser) context.toast({ kind: "success", title: t.reconnected });
    });
    connection.addEventListener("message", (event) => {
      if (!active()) return;
      const stick = nearBottom();
      try {
        receive(event.data);
      } catch {
        if (!warned) context.toast({ kind: "error", title: t.unreadable });
        warned = true;
      }
      scroll(stick);
    });
    const lost = () => {
      if (active()) disconnected();
    };
    connection.addEventListener("error", lost);
    connection.addEventListener("close", lost);
  }

  function disconnected() {
    socket = undefined;
    setOnline(false);
    input.disabled = true;
    show(typing, false);
    caret.remove();
    const count = [...sent.values()].filter((entry) => !entry.confirmed).length;
    offlineCount.replaceChildren(
      count
        ? h(
            "span",
            null,
            " · ",
            term(fill(t.unconfirmed, { n: count }), t.unconfirmedTip, lang),
          )
        : "",
    );
    show(offline, true);
  }

  signal.addEventListener(
    "abort",
    () => {
      const connection = socket;
      socket = undefined;
      connection?.close();
      for (const url of urls) URL.revokeObjectURL(url);
      urls.clear();
      agents.clear();
      sent.clear();
      input.value = "";
    },
    { once: true },
  );

  column.append(empty, typing);
  changed();
  connect(false);
  return h(
    "div",
    { style: "flex:1 1 auto;display:flex;flex-direction:column;min-width:0" },
    log,
    form,
  );
});
