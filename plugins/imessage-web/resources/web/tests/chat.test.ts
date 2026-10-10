import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  installBrowser,
  settle,
} from "../../../../captive-portal/resources/web/tests/browser";
import type {
  PageBadge,
  PortalContext,
  Toast,
} from "../../../../captive-portal/resources/web/ui";
import { mount } from "../entry";

let browser: ReturnType<typeof installBrowser>;
let sockets: Socket[];
let sent: string[];
let failSend: boolean;
const previous = globalThis.WebSocket;
/** Bun's own Event: the fake socket is a Bun EventTarget, while happy-dom replaces the global. */
const NativeEvent = globalThis.Event;

class Socket extends EventTarget {
  static OPEN = 1;
  readyState = 0;
  bufferedAmount = 0;
  constructor(public url: string | URL) {
    super();
    sockets.push(this);
  }
  send(text: string) {
    if (failSend) throw new Error("closed");
    sent.push(text);
  }
  close() {
    this.readyState = 3;
  }
  open() {
    this.readyState = 1;
    this.dispatchEvent(new NativeEvent("open"));
  }
  emit(event: string, data: unknown) {
    this.dispatchEvent(
      new MessageEvent("message", {
        data: `id: 1\nevent: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
      }),
    );
  }
  agent(id: string, type: string, payload: unknown = {}) {
    this.emit("message.event", {
      message_id: id,
      payload,
      sequence: 1,
      session: "session-1",
      type,
    });
  }
}

beforeEach(() => {
  browser = installBrowser();
  sockets = [];
  sent = [];
  failSend = false;
  Object.assign(globalThis, { WebSocket: Socket });
});
afterEach(async () => {
  Object.assign(globalThis, { WebSocket: previous });
  await browser.close();
});

function open(lang: "zh" | "en" = "zh") {
  const controller = new AbortController();
  const toasts: Toast[] = [];
  const badges: (PageBadge | null)[] = [];
  const context: PortalContext = {
    signal: controller.signal,
    lang,
    toast: (toast) => {
      if (!controller.signal.aborted) toasts.push(toast);
    },
    navigate: () => {},
    status: () => null,
    refreshStatus: async () => {},
    badge: (badge) => badges.push(badge),
  };
  const root = browser.document.createElement("div");
  browser.document.body.append(root);
  const cleanup = mount(root, context) as (() => void) | undefined;
  const socket = () => sockets.at(-1)!;
  const input = root.querySelector("textarea")!;
  const type = (text: string) => {
    input.value = text;
    input.dispatchEvent(new browser.window.Event("input"));
  };
  const submit = (text: string) => {
    type(text);
    root
      .querySelector("form")!
      .dispatchEvent(new browser.window.Event("submit", { cancelable: true }));
  };
  const button = (label: string) =>
    [...root.querySelectorAll("button")].find(
      (node) =>
        node.getAttribute("aria-label") === label ||
        node.textContent?.trim() === label,
    )!;
  const send = () =>
    root.querySelector<HTMLButtonElement>("button[type=submit]")!;
  return {
    root,
    controller,
    toasts,
    badges,
    socket,
    input,
    type,
    submit,
    button,
    send,
    cleanup,
  };
}

/** One finished Agent turn answering `reply_to`, streaming `text`. */
function reply(ws: Socket, id: string, replyTo: string, text: string) {
  ws.emit("message.start", {
    kind: "reply",
    message_id: id,
    reply_to: replyTo,
  });
  ws.agent(id, "output_delta", { text });
  ws.agent(id, "output_ended");
  ws.emit("message.end", { error: null, message_id: id });
}

test("opens on a fresh temporary session with the composer mid-page", () => {
  const { root, socket, send, type } = open();
  expect(String(socket().url)).toBe("ws://localhost/ws/message");
  const page = root.firstElementChild as HTMLElement;
  expect(page.classList.contains("bc-chat--fresh")).toBe(true);
  expect(root.querySelector("[role=log]")!.className).toBe("bc-chat-log");
  expect(root.querySelector("[role=log]")!.getAttribute("aria-label")).toBe(
    "聊天记录",
  );
  expect(root.querySelector("form")!.className).toBe("bc-chat-dock");
  // the EmptyState: the page's laptop figure, the session's name and one lead sentence
  const empty = root.querySelector(".bc-empty")!;
  expect(empty.querySelector("hl-figure")?.getAttribute("name")).toBe(
    "imessage-web",
  );
  expect(empty.querySelector(".bc-title")?.textContent).toBe("临时会话");
  expect(empty.textContent).toContain("刷新页面后从空白开始。");
  // suggestions sit under the composer; the pinned head waits for the first message
  expect(root.querySelector(".bc-chat-dock")!.textContent).toContain(
    "今天适合骑车吗？",
  );
  expect(root.querySelector<HTMLElement>(".bc-chat-head")!.hidden).toBe(true);
  // one row: the textarea and its actions; no counter, no term in the composer
  const row = root.querySelector(".bc-composer__row")!;
  expect(row.firstElementChild?.tagName).toBe("TEXTAREA");
  expect(row.querySelector("textarea")!.getAttribute("rows")).toBe("1");
  expect(row.lastElementChild?.className).toBe("bc-composer__actions");
  expect(root.querySelector(".bc-composer")!.textContent).not.toContain(
    "bytes",
  );
  expect(root.querySelector(".bc-composer .bc-term")).toBeNull();
  // send is an icon button named by its tooltip, grey until the socket is open and there is text
  expect(send().getAttribute("aria-label")).toBe("发送");
  expect(send().querySelector(".bc-tooltip")?.textContent).toBe("发送");
  expect(send().disabled).toBe(true);
  socket().open();
  expect(send().disabled).toBe(true);
  type("你好");
  expect(send().disabled).toBe(false);
});

test("renders in English", () => {
  const { root, input } = open("en");
  expect(root.querySelector(".bc-empty .bc-title")?.textContent).toBe(
    "Temporary session",
  );
  expect(root.textContent).toContain("a reload starts from blank.");
  expect(input.placeholder).toBe("Message the device");
  expect(root.textContent).not.toContain("临时会话");
});

test("sends WebClientFrame JSON; past the limit send turns grey and one sentence says why", () => {
  const { root, socket, submit, type, input, send } = open();
  socket().open();
  submit("   ");
  expect(sent).toHaveLength(0);
  type("字".repeat(400));
  expect(input.getAttribute("aria-invalid")).toBe("true");
  expect(send().disabled).toBe(true);
  expect(root.querySelector(".bc-hint--error")?.textContent).toBe(
    "消息太长，请缩短后发送。",
  );
  // nothing counts: no bytes, no ring
  expect(root.textContent).not.toContain("1024");
  submit("字".repeat(400));
  expect(sent).toHaveLength(0);
  submit("hello");
  expect(JSON.parse(sent[0])).toEqual({ text: "hello" });
  expect(input.value).toBe("");
  expect(input.hasAttribute("aria-invalid")).toBe(false);
  expect(root.querySelector(".bc-bubble")!.textContent).toBe("hello");
  expect(root.textContent).toContain("已发送");
  // the conversation began: the composer docks and the head pins 「临时会话」 with its tooltip
  const page = root.firstElementChild as HTMLElement;
  expect(page.classList.contains("bc-chat--fresh")).toBe(false);
  const head = root.querySelector<HTMLElement>(".bc-chat-head")!;
  expect(head.hidden).toBe(false);
  expect(
    head.querySelector(".bc-badge")?.firstChild?.nextSibling?.textContent,
  ).toBe("临时会话");
  expect(head.querySelector(".bc-tooltip")?.textContent).toBe(
    "只显示连接后的消息，刷新页面后清空",
  );
  expect(root.querySelector(".bc-empty")).toBeNull();
});

test("a suggestion sends itself", () => {
  const { socket, button } = open();
  socket().open();
  button("提醒我 6 点下班").click();
  expect(JSON.parse(sent[0])).toEqual({ text: "提醒我 6 点下班" });
});

test("while the device works, 「思考中」 follows a turn of its own, then the running turn", () => {
  const { root, socket, submit } = open();
  const ws = socket();
  ws.open();
  submit("天气？");
  // asked and not begun: an author line over 「思考中」 (the dots stand in for the mark without WebGL)
  const shimmer = () => root.querySelector(".bc-shimmer");
  expect(shimmer()?.textContent).toBe("思考中");
  const pending = shimmer()!.closest(".bc-turn")!;
  expect(pending.querySelector(".bc-meta--author")?.textContent).toBe(
    "Barracuda",
  );
  expect(pending.querySelectorAll(".bc-typing > i")).toHaveLength(3);
  // begun: 「思考中」 moves under the running turn's own author line
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-1",
    reply_to: "web-in-1",
  });
  expect(root.querySelectorAll(".bc-meta--author")).toHaveLength(1);
  expect(shimmer()?.closest(".bc-turn")).toBe(
    root.querySelector(".bc-meta--author")!.closest(".bc-turn"),
  );
  // streaming text speaks for itself
  ws.agent("web-1", "output_delta", { text: "晴" });
  expect(shimmer()).toBeNull();
  ws.agent("web-1", "output_ended");
  ws.emit("message.end", { error: null, message_id: "web-1" });
  expect(shimmer()).toBeNull();
  expect(root.querySelector(".bc-typing")).toBeNull();
});

test("renders an Agent turn: reasoning, tool card, streamed reply and an icon footer", () => {
  const { root, socket, submit, button } = open();
  const ws = socket();
  ws.open();
  submit("天气？");
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-1",
    reply_to: "web-in-1",
  });
  ws.agent("web-1", "turn_started", { turn: "turn-1", origin: "user" });
  ws.agent("web-1", "iteration_started", { iteration: "1" });
  ws.agent("web-1", "reasoning_delta", { text: "先搜索" });
  // reasoning opens under a shimmering 「思考中」 while it streams
  const fold = root.querySelector<HTMLButtonElement>(".bc-fold-link")!;
  expect(fold.getAttribute("aria-expanded")).toBe("true");
  expect(fold.querySelector(".bc-shimmer")?.textContent).toBe("思考中");
  ws.agent("web-1", "reasoning_ended");
  // then folds itself away, named for what it was
  expect(fold.getAttribute("aria-expanded")).toBe("false");
  expect(fold.textContent?.trim()).toBe("思考过程");
  expect(fold.querySelector(".bc-shimmer")).toBeNull();
  ws.agent("web-1", "tool_result_started");
  ws.agent("web-1", "tool_call_id_delta", { text: "c1" });
  ws.agent("web-1", "tool_name_delta", { text: "weather_forecast" });
  ws.agent("web-1", "tool_arguments_delta", { text: '{"city":"shenzhen"}' });
  // a running tool spins 「运行中」
  const tool = root.querySelector(".bc-fold")!;
  expect(tool.querySelector(".bc-status")?.textContent).toBe("运行中");
  expect(tool.querySelector(".bc-status .bc-spinner")).not.toBeNull();
  ws.agent("web-1", "tool_output_delta", { text: "HTTP 503" });
  ws.agent("web-1", "tool_result_ended", { ok: false });
  ws.agent("web-1", "output_delta", { text: "<img src=x>" });
  ws.agent("web-1", "output_delta", { text: " 晴" });
  // streaming: a caret, no footer yet
  expect(root.querySelector(".bc-reply .bc-caret")).not.toBeNull();
  expect(button("复制")).toBeUndefined();
  ws.agent("web-1", "usage", { input_tokens: 1284, output_tokens: 200 });
  ws.agent("web-1", "output_ended");
  ws.agent("web-1", "turn_ended", { turn: "turn-1" });
  ws.emit("message.end", { error: null, message_id: "web-1" });

  expect(root.querySelector("img")).toBeNull();
  expect(root.querySelector(".bc-reply")!.textContent).toBe("<img src=x> 晴");
  expect(root.querySelector(".bc-reply .bc-caret")).toBeNull();
  fold.click();
  expect(root.textContent).toContain("先搜索");
  expect(root.querySelector("p.bc-quote")?.textContent).toBe("先搜索");
  expect(tool.textContent).toContain("weather_forecast");
  expect(tool.querySelector(".bc-status")?.className).toBe(
    "bc-status bc-destructive",
  );
  // a failed tool opens itself
  expect(tool.getAttribute("aria-expanded")).toBe("true");
  expect(tool.nextElementSibling?.className).toBe(
    "bc-card__body bc-kv bc-kv--dense",
  );
  expect(tool.querySelector(".bc-tool__args")?.textContent).toBe(
    '{"city":"shenzhen"}',
  );
  expect(root.textContent).toContain("HTTP 503");
  // the footer holds only its actions, icon buttons named by tooltips; no run statistics
  const copy = button("复制");
  const respond = button("回复");
  expect(copy.className).toBe("bc-icon-button");
  expect(copy.querySelector(".bc-tooltip")?.textContent).toBe("复制");
  expect(respond.className).toBe("bc-icon-button");
  expect(copy.parentElement).toBe(respond.parentElement);
  expect(copy.parentElement?.classList.contains("bc-meta")).toBe(true);
  expect(root.textContent).not.toContain("tokens");
  expect(root.textContent).not.toContain("第 1 步");
  // the chevrons turn in CSS from aria-expanded; nothing transforms them inline
  for (const svg of root.querySelectorAll("svg"))
    expect(svg.getAttribute("style") ?? "").not.toContain("transform");

  // reply to the turn: the quoted bar, cancelled with its icon button
  respond.click();
  expect(root.textContent).toContain("回复 Barracuda");
  expect(button("取消回复").className).toBe("bc-icon-button");
  submit("后天呢？");
  // the frame stays `{ text }`: the quote rides inside it as escaped XML
  expect(JSON.parse(sent[1])).toEqual({
    text: "<quote>&lt;img src=x&gt; 晴</quote>\n后天呢？",
  });
  expect(
    root.querySelectorAll(".bc-turn--user .bc-quote")[0]?.textContent,
  ).toBe("<img src=x> 晴");
  expect(root.querySelectorAll(".bc-bubble")).toHaveLength(2);

  // edit, reaction, delete
  ws.emit("message.edit", { message_id: "web-1", text: "多云" });
  expect(root.querySelector(".bc-reply")!.textContent).toContain("多云");
  expect(root.textContent).toContain("已编辑");
  ws.emit("message.reaction", { message_id: "web-in-1", reaction: "👍" });
  expect(root.textContent).toContain("👍");
  ws.emit("message.delete", { message_id: "web-1" });
  expect(root.textContent).not.toContain("多云");
  expect(root.textContent).not.toContain("weather_forecast");
});

test("a long quote is cut to 512 bytes, whole characters, ending in an ellipsis", () => {
  const { socket, submit, button } = open();
  const ws = socket();
  ws.open();
  submit("讲个故事");
  // 171 three-byte characters are 513 bytes: one too many
  reply(ws, "web-1", "web-in-1", "长".repeat(171));
  button("回复").click();
  submit("然后呢？");
  const { text } = JSON.parse(sent[1]) as { text: string };
  expect(text).toBe(`<quote>${"长".repeat(169)}…</quote>\n然后呢？`);
});

test("a message written while a turn runs waits in the queue, editable, then goes out", () => {
  const { root, socket, submit, button, input, send } = open();
  const ws = socket();
  ws.open();
  submit("一");
  expect(sent).toHaveLength(1);
  // the device has not begun: the next message queues instead of going out
  submit("二");
  submit("三");
  expect(sent).toHaveLength(1);
  const queue = root.querySelector("[role=list]")!;
  expect(queue.textContent).toContain("排队中 · 02");
  expect(
    [...queue.querySelectorAll("[role=listitem] .bc-composer__text")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["二", "三"]);
  input.value = "x";
  input.dispatchEvent(new browser.window.Event("input"));
  expect(send().getAttribute("aria-label")).toBe("加入队列");
  input.value = "";
  input.dispatchEvent(new browser.window.Event("input"));
  // edit takes a queued message back into the composer; remove drops it
  button("编辑").click();
  expect(input.value).toBe("二");
  button("移除").click();
  expect(queue.textContent).not.toContain("三");
  expect((queue as HTMLElement).hidden).toBe(true);
  submit("二");
  expect(sent).toHaveLength(1);
  // the turn ends: the oldest queued message goes out by itself
  reply(ws, "web-1", "web-in-1", "好");
  expect(sent.map((frame) => JSON.parse(frame).text)).toEqual(["一", "二"]);
  expect(send().getAttribute("aria-label")).toBe("加入队列");
  reply(ws, "web-2", "web-in-2", "好");
  expect(send().getAttribute("aria-label")).toBe("发送");
});

test("stop interrupts the running turn, which ends 「已停止」 and keeps its text", () => {
  const { root, socket, submit, button, type, send } = open();
  const ws = socket();
  ws.open();
  const stop = () => button("停止");
  // idle: send, no stop
  expect(stop().hidden).toBe(true);
  submit("写一份清单");
  // a turn runs: stop is the primary, send steps aside until the reader types
  expect(stop().hidden).toBe(false);
  expect(stop().className).toBe("bc-button bc-button--icon");
  expect(send().hidden).toBe(true);
  type("只要 5 条");
  expect(stop().className).toBe("bc-button bc-button--icon bc-button--outline");
  expect(send().hidden).toBe(false);
  expect(send().getAttribute("aria-label")).toBe("加入队列");
  type("");
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-1",
    reply_to: "web-in-1",
  });
  ws.agent("web-1", "output_delta", { text: "1. 断电恢复" });
  stop().click();
  // a control, not a message: it takes no id and draws nothing
  expect(JSON.parse(sent.at(-1)!)).toEqual({ control: "interrupt" });
  expect(root.querySelectorAll(".bc-bubble")).toHaveLength(1);
  expect(stop().disabled).toBe(true);
  ws.agent("web-1", "output_ended");
  ws.agent("web-1", "turn_ended", { turn: "turn-1", outcome: "interrupted" });
  ws.emit("message.end", { error: null, message_id: "web-1" });
  const status = root.querySelector(".bc-status.bc-muted")!;
  expect(status.textContent).toBe("已停止");
  expect(root.querySelector(".bc-reply")!.textContent).toBe("1. 断电恢复");
  // its footer follows the status line
  expect(
    status.nextElementSibling?.querySelector('[aria-label="复制"]'),
  ).not.toBeNull();
  expect(stop().hidden).toBe(true);
  expect(send().hidden).toBe(false);
  // a completed turn says nothing
  submit("再来");
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-2",
    reply_to: "web-in-2",
  });
  ws.agent("web-2", "turn_ended", { turn: "turn-2", outcome: "completed" });
  ws.emit("message.end", { error: null, message_id: "web-2" });
  expect(root.querySelectorAll(".bc-status.bc-muted")).toHaveLength(1);
});

test("stop asked before the turn begins is asked again once it does", () => {
  const { socket, submit, button } = open();
  const ws = socket();
  ws.open();
  submit("一");
  button("停止").click();
  expect(sent.slice(1).map((frame) => JSON.parse(frame))).toEqual([
    { control: "interrupt" },
  ]);
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-1",
    reply_to: "web-in-1",
  });
  expect(sent.slice(1).map((frame) => JSON.parse(frame))).toEqual([
    { control: "interrupt" },
    { control: "interrupt" },
  ]);
});

test("rewind cancels the running turn and puts the message back in the composer", () => {
  const { root, socket, submit, input, type } = open();
  const ws = socket();
  ws.open();
  const rewind = () =>
    [
      ...root.querySelectorAll<HTMLButtonElement>('button[aria-label="撤回"]'),
    ].filter((node) => !node.hidden);
  submit("写一份清单");
  reply(ws, "web-1", "web-in-1", "好的");
  // a finished turn cannot be rewound
  expect(rewind()).toHaveLength(0);
  submit("只要最重要的 5 条");
  // waiting on the device, and then running: the message carries 「撤回」
  expect(rewind()).toHaveLength(1);
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-2",
    reply_to: "web-in-2",
  });
  ws.agent("web-2", "output_delta", { text: "1. 断电" });
  expect(rewind()).toHaveLength(1);
  expect(rewind()[0].closest(".bc-turn--user")?.textContent).toContain(
    "只要最重要的 5 条",
  );
  // typed text stays, after the withdrawn message
  type("按风险排序");
  submit("按风险排序");
  expect(root.querySelector("[role=list]")!.textContent).toContain(
    "排队中 · 01",
  );
  rewind()[0].click();
  expect(JSON.parse(sent.at(-1)!)).toEqual({ control: "cancel" });
  // the message and its turn leave the log; what came before stays
  expect(
    [...root.querySelectorAll(".bc-bubble")].map((node) => node.textContent),
  ).toEqual(["写一份清单"]);
  expect(root.textContent).not.toContain("1. 断电");
  expect(root.querySelectorAll(".bc-meta--author")).toHaveLength(1);
  expect(input.value).toBe("只要最重要的 5 条");
  // the cancelled turn's last events draw nothing
  ws.agent("web-2", "output_delta", { text: "2. 校验" });
  ws.agent("web-2", "turn_ended", { turn: "turn-2", outcome: "cancelled" });
  ws.emit("message.end", { error: null, message_id: "web-2" });
  expect(root.textContent).not.toContain("校验");
  expect(root.textContent).not.toContain("已停止");
  // the queue waits for the withdrawn text, which goes first
  expect(JSON.parse(sent.at(-1)!)).toEqual({ control: "cancel" });
  submit("只要最重要的 3 条");
  expect(JSON.parse(sent.at(-1)!)).toEqual({ text: "只要最重要的 3 条" });
  reply(ws, "web-3", "web-in-3", "好");
  expect(JSON.parse(sent.at(-1)!)).toEqual({ text: "按风险排序" });
});

test("rewinding the only message returns to a fresh conversation; a late start is cancelled again", () => {
  const { root, socket, submit, input } = open();
  const ws = socket();
  ws.open();
  submit("天气？");
  root.querySelector<HTMLButtonElement>('button[aria-label="撤回"]')!.click();
  expect(JSON.parse(sent.at(-1)!)).toEqual({ control: "cancel" });
  expect(root.querySelector(".bc-bubble")).toBeNull();
  expect(
    (root.firstElementChild as HTMLElement).classList.contains(
      "bc-chat--fresh",
    ),
  ).toBe(true);
  expect(input.value).toBe("天气？");
  // the device had not begun when the cancel came: its turn starts anyway, and is cancelled again
  ws.emit("message.start", {
    kind: "reply",
    message_id: "web-1",
    reply_to: "web-in-1",
  });
  expect(JSON.parse(sent.at(-1)!)).toEqual({ control: "cancel" });
  ws.agent("web-1", "output_delta", { text: "晴" });
  ws.emit("message.end", { error: null, message_id: "web-1" });
  expect(root.querySelector(".bc-meta--author")).toBeNull();
  expect(root.textContent).not.toContain("晴");
});

test("a permission request is answered by the next message", () => {
  const { root, socket, button, input } = open();
  const ws = socket();
  ws.open();
  ws.emit("message.start", { kind: "reply", message_id: "web-2" });
  ws.agent("web-2", "input_request_started", {
    request: "request-1",
    kind: "permission_approval",
  });
  ws.agent("web-2", "input_request_tool_name_delta", { text: "fs.remove" });
  ws.agent("web-2", "input_request_arguments_delta", { text: '{"count":12}' });
  ws.agent("web-2", "input_request_reason_delta", { text: "清理旧笔记" });
  expect(root.textContent).not.toContain("正在答复");
  ws.agent("web-2", "input_requested", { request: "request-1" });
  expect(root.textContent).toContain("需要你的允许");
  expect(root.textContent).toContain('{"count":12}');
  expect(input.placeholder).toBe("回复这次请求…");
  // a question waiting on the reader is not 「思考中」
  expect(root.querySelector(".bc-shimmer")).toBeNull();
  const approval = root.querySelector("section.bc-card")!;
  expect([...approval.children].map((node) => node.className)).toEqual([
    "bc-card__head",
    "bc-card__body bc-kv bc-kv--dense",
    "bc-card__foot",
  ]);
  expect(approval.querySelector("dd.bc-tool__name")?.textContent).toBe(
    "fs.remove",
  );
  expect(
    root.querySelector(".bc-composer__bar.bc-composer__bar--rule")?.textContent,
  ).toBe("正在答复 · fs.remove");
  const allow = button("允许");
  allow.click();
  // the answer goes out at once, even though the turn still runs
  expect(JSON.parse(sent[0])).toEqual({ text: "允许" });
  expect(allow.disabled).toBe(true);
  expect(root.textContent).not.toContain("正在答复");
  expect(input.placeholder).toBe("给设备发一条消息");
});

test("shows lag, turn errors, interrupted messages and attachments", async () => {
  const { root, socket } = open();
  const ws = socket();
  ws.open();
  ws.emit("stream.lagged", { missed: 3 });
  expect(root.querySelector("[role=separator]")!.textContent).toBe(
    "错过 3 个事件",
  );
  ws.emit("message.start", { kind: "reply", message_id: "web-3" });
  ws.agent("web-3", "output_delta", { text: "会议要点：" });
  ws.agent("web-3", "turn_error", {
    message: "429 Too Many Requests",
    message_truncated: false,
  });
  ws.emit("message.end", { error: "worker_stopped", message_id: "web-3" });
  expect(root.textContent).toContain("这一轮出错");
  expect(root.textContent).toContain("429 Too Many Requests");
  expect(root.textContent).toContain("未完成 · 连接中断");
  // the device's internal reason stays off the page
  expect(root.textContent).not.toContain("worker_stopped");
  expect(
    root.querySelector("[role=log] .bc-alert--error > .bc-alert__body")
      ?.firstElementChild?.textContent,
  ).toBe("这一轮出错");

  ws.emit("message.file", {
    caption: "路线",
    filename: "ride.gpx",
    message_id: "web-4",
    mime_type: "application/gpx+xml",
    phase: "start",
    reply_to: null,
  });
  expect(root.textContent).toContain("接收中");
  ws.emit("message.file", {
    data: btoa("hello"),
    message_id: "web-4",
    phase: "delta",
  });
  ws.emit("message.file", { error: null, message_id: "web-4", phase: "end" });
  const file = root.querySelector(".bc-card > .bc-card__row")!;
  expect(file.querySelector(".bc-option-title")?.textContent).toBe("ride.gpx");
  expect(file.querySelector(".bc-caption.bc-muted")?.textContent).toBe(
    "application/gpx+xml · 5 B · 已接收",
  );
  const link = root.querySelector<HTMLAnchorElement>("a[download]")!;
  expect(link.getAttribute("download")).toBe("ride.gpx");
  expect(await (await fetch(link.href)).text()).toBe("hello");
  expect(root.textContent).toContain("路线");
});

test("a dropped connection counts unconfirmed sends, keeps the queue and reconnects on request", () => {
  const { root, socket, submit, input, button, toasts, badges } = open();
  socket().open();
  // the top bar shows the connection, as the design's chat boards do
  expect(badges.at(-1)).toEqual({ label: "已连接", tone: "live" });
  submit("一");
  submit("二");
  // 「二」 waits in the page's queue; only 「一」 went out, and its turn never started
  expect(sent).toHaveLength(1);
  socket().dispatchEvent(new NativeEvent("close"));
  expect(badges.at(-1)).toEqual({ label: "已断开", tone: "lost" });
  expect(root.textContent).toContain(
    "连接已中断 · 1 条消息未确认，重新连接后再发送",
  );
  expect(input.disabled).toBe(true);
  expect(toasts).toHaveLength(0);
  const banner = root.querySelector(".bc-chat-dock .bc-alert")!;
  expect(banner.className).toBe("bc-alert bc-alert--error bc-alert--compact");
  button("重新连接").click();
  expect(sockets).toHaveLength(2);
  socket().open();
  expect(input.disabled).toBe(false);
  expect(toasts).toEqual([{ kind: "success", title: "已重新连接" }]);
  // the queued message goes out once the device is reachable again
  expect(JSON.parse(sent.at(-1)!)).toEqual({ text: "二" });
});

test("toasts send failures and unreadable frames", () => {
  const { socket, submit, toasts, input } = open("en");
  socket().open();
  failSend = true;
  submit("hello");
  expect(toasts).toEqual([{ kind: "error", title: "Message not sent" }]);
  expect(input.value).toBe("hello");
  socket().dispatchEvent(
    new MessageEvent("message", { data: "event: bad\ndata: {\n\n" }),
  );
  socket().dispatchEvent(
    new MessageEvent("message", { data: "event: bad\ndata: {\n\n" }),
  );
  expect(toasts.slice(1)).toEqual([
    { kind: "error", title: "Received a message this page can't read" },
  ]);
});

test("closes the socket and empties the page when it goes away", async () => {
  const { root, socket, controller, toasts, cleanup } = open();
  socket().open();
  controller.abort();
  cleanup?.();
  await settle();
  expect(root.childElementCount).toBe(0);
  expect(socket().readyState).toBe(3);
  socket().dispatchEvent(new NativeEvent("close"));
  expect(toasts).toHaveLength(0);
});
