import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  installBrowser,
  settle,
} from "../../../../captive-portal/resources/web/tests/browser";
import type {
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
  const context: PortalContext = {
    signal: controller.signal,
    lang,
    toast: (toast) => {
      if (!controller.signal.aborted) toasts.push(toast);
    },
    navigate: () => {},
    status: () => null,
    refreshStatus: async () => {},
  };
  const root = browser.document.createElement("div");
  browser.document.body.append(root);
  const cleanup = mount(root, context) as (() => void) | undefined;
  const socket = () => sockets.at(-1)!;
  const input = root.querySelector("textarea")!;
  const submit = (text: string) => {
    input.value = text;
    input.dispatchEvent(new browser.window.Event("input"));
    root
      .querySelector("form")!
      .dispatchEvent(new browser.window.Event("submit", { cancelable: true }));
  };
  const button = (label: string) =>
    [...root.querySelectorAll("button")].find(
      (node) => node.textContent?.trim() === label,
    )!;
  return { root, controller, toasts, socket, input, submit, button, cleanup };
}

test("renders the empty conversation in Chinese and connects to the bridge", () => {
  const { root, socket } = open();
  expect(String(socket().url)).toBe("ws://localhost/ws/message");
  expect(root.textContent).toContain("还没有消息");
  expect(root.textContent).toContain("今天适合骑车吗？");
  expect(root.textContent).toContain("临时会话");
  expect(root.textContent).toContain("0 / 1024 bytes");
  expect(root.querySelector("[role=log]")!.getAttribute("aria-label")).toBe(
    "聊天记录",
  );
  const send = root.querySelector<HTMLButtonElement>("button[type=submit]")!;
  expect(send.disabled).toBe(true);
  socket().open();
  expect(send.disabled).toBe(false);
});

test("renders in English", () => {
  const { root, input } = open("en");
  expect(root.textContent).toContain("No messages yet");
  expect(root.textContent).toContain("Temporary session");
  expect(input.placeholder).toBe("Message the device");
  expect(root.textContent).not.toContain("还没有消息");
});

test("sends WebClientFrame JSON and validates the byte limit", () => {
  const { root, socket, submit, input } = open();
  socket().open();
  submit("   ");
  expect(sent).toHaveLength(0);
  submit("字".repeat(400));
  expect(sent).toHaveLength(0);
  expect(input.getAttribute("aria-invalid")).toBe("true");
  expect(root.textContent).toContain("消息超过 1024 字节");
  submit("hello");
  expect(JSON.parse(sent[0])).toEqual({ text: "hello" });
  expect(input.value).toBe("");
  expect(input.hasAttribute("aria-invalid")).toBe(false);
  expect(root.querySelector(".bc-bubble")!.textContent).toBe("hello");
  expect(root.textContent).toContain("已发送");
  // 已发送 is plain status text: the composer's 临时会话 is this screen's term
  expect(
    [...root.querySelectorAll(".bc-term")].map(
      (node) => node.firstChild?.textContent,
    ),
  ).toEqual(["临时会话"]);
  expect(root.textContent).not.toContain("还没有消息");
});

test("a suggestion sends itself", () => {
  const { socket, button } = open();
  socket().open();
  button("提醒我 6 点下班").click();
  expect(JSON.parse(sent[0])).toEqual({ text: "提醒我 6 点下班" });
});

test("renders an Agent turn: reasoning, tool card, streamed reply and footer", () => {
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
  ws.agent("web-1", "reasoning_ended");
  ws.agent("web-1", "tool_result_started");
  ws.agent("web-1", "tool_call_id_delta", { text: "c1" });
  ws.agent("web-1", "tool_name_delta", { text: "weather_forecast" });
  ws.agent("web-1", "tool_arguments_delta", { text: '{"city":"shenzhen"}' });
  ws.agent("web-1", "tool_output_delta", { text: "HTTP 503" });
  ws.agent("web-1", "tool_result_ended", { ok: false });
  ws.agent("web-1", "output_delta", { text: "<img src=x>" });
  ws.agent("web-1", "output_delta", { text: " 晴" });
  ws.emit("conversation.typing", { typing: true });
  expect(root.textContent).toContain("正在输入");
  // streaming: a caret, no footer yet
  expect(root.querySelector(".bc-reply [aria-hidden=true]")).not.toBeNull();
  expect(root.textContent).not.toContain("第 1 步");
  ws.agent("web-1", "usage", { input_tokens: 1284, output_tokens: 200 });
  ws.agent("web-1", "usage", { output_tokens: 12 });
  ws.agent("web-1", "output_ended");
  ws.agent("web-1", "turn_ended", { turn: "turn-1" });
  ws.emit("message.end", { error: null, message_id: "web-1" });
  ws.emit("conversation.typing", { typing: false });

  expect(root.querySelector("img")).toBeNull();
  expect(root.querySelector(".bc-reply")!.textContent).toBe("<img src=x> 晴");
  expect(root.querySelector(".bc-reply [aria-hidden=true]")).toBeNull();
  const reasoning = button("思考过程");
  expect(reasoning.getAttribute("aria-expanded")).toBe("false");
  reasoning.click();
  expect(reasoning.getAttribute("aria-expanded")).toBe("true");
  expect(root.textContent).toContain("先搜索");
  const tool = root.querySelector(".bc-fold")!;
  expect(tool.textContent).toContain("weather_forecast");
  expect(tool.textContent).toContain("失败");
  // a failed tool opens itself
  expect(tool.getAttribute("aria-expanded")).toBe("true");
  expect(root.textContent).toContain("HTTP 503");
  expect(root.textContent).toContain("第 1 步 · 输入 1,284 · 输出 212 tokens");
  // words in sans, the numbers and the unit in mono
  expect(
    [...root.querySelectorAll(".bc-reply ~ div .bc-mono")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["1", "1,284", "212 tokens"]);

  // reply to the turn: the design's inline action, cancelled with the small icon button
  expect(button("回复").className).toBe("bc-fold-link");
  button("回复").click();
  expect(root.textContent).toContain("回复 Barracuda");
  expect(
    root.querySelector(".bc-icon-button")?.getAttribute("aria-label"),
  ).toBe("取消回复");
  submit("后天呢？");
  expect(JSON.parse(sent[1])).toEqual({ text: "后天呢？", reply_to: "web-1" });
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
  expect(root.textContent).toContain("正在答复 · fs.remove");
  expect(input.placeholder).toBe("回复这次请求…");
  const allow = button("允许");
  allow.click();
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
  // the failed turn is a record in the log
  expect(
    root.querySelector("[role=log] .bc-alert--error")?.textContent,
  ).toContain("这一轮出错");

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
  expect(root.textContent).toContain("application/gpx+xml · 5 B · 已接收");
  const link = root.querySelector<HTMLAnchorElement>("a[download]")!;
  expect(link.getAttribute("download")).toBe("ride.gpx");
  expect(await (await fetch(link.href)).text()).toBe("hello");
  expect(root.textContent).toContain("路线");
});

test("a dropped connection counts unconfirmed sends and reconnects on request", () => {
  const { root, socket, submit, input, button, toasts } = open();
  socket().open();
  submit("一");
  submit("二");
  socket().emit("message.start", {
    kind: "reply",
    message_id: "web-1",
    reply_to: "web-in-1",
  });
  socket().dispatchEvent(new NativeEvent("close"));
  expect(root.textContent).toContain(
    "连接已中断 · 1 条消息未确认，重新连接后再发送",
  );
  expect(input.disabled).toBe(true);
  expect(toasts).toHaveLength(0);
  button("重新连接").click();
  expect(sockets).toHaveLength(2);
  socket().open();
  expect(input.disabled).toBe(false);
  const offline = [...root.querySelectorAll<HTMLElement>(".bc-alert")].find(
    (node) => node.textContent?.includes("连接已中断"),
  )!;
  expect(offline.hidden).toBe(true);
  expect(toasts).toEqual([{ kind: "success", title: "已重新连接" }]);
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
