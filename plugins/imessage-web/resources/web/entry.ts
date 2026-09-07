import { element, panel } from "../../../captive-portal/resources/web/ui/form";

export function mount(root: HTMLElement, { signal }: { signal: AbortSignal }) {
  if (signal.aborted) return;
  const page = panel(
    root,
    "Web 聊天",
    "连接设备的 Web 消息通道。只接收连接后的消息；断线不会自动重发。当前支持文本，附件仅提示。页面不持久保存聊天记录。",
  );
  const status = element("p", "正在连接…");
  page.append(
    element(
      "style",
      ".chat-history{max-height:48vh;min-height:160px;overflow-y:auto;margin:24px 0;border-block:1px solid #dce4e0}.chat-history p{white-space:pre-wrap;overflow-wrap:anywhere;padding:14px 18px;border-radius:12px;background:#edf6f1}.chat-history small{display:block;color:#9f302e}",
    ),
  );
  status.setAttribute("role", "status");
  const reconnect = element("button", "重新连接");
  reconnect.type = "button";
  const history = element("div");
  history.className = "chat-history";
  history.setAttribute("role", "log");
  history.setAttribute("aria-label", "聊天记录");
  const form = element("form");
  const label = element("label", "消息（最多 1024 UTF-8 字节）");
  const input = element("textarea");
  input.required = true;
  input.rows = 3;
  input.maxLength = 1024;
  label.append(input);
  const send = element("button", "发送");
  send.type = "submit";
  send.disabled = true;
  form.append(label, send);
  page.append(status, reconnect, history, form);
  const lifecycle = new AbortController();
  const messages = new Map<string, HTMLElement>();
  let socket: WebSocket | undefined;
  let localId = 0;
  function message(id: string) {
    let node = messages.get(id);
    if (!node) {
      node = element("p");
      messages.set(id, node);
      history.append(node);
      if (messages.size > 100) {
        const first = messages.keys().next().value!;
        messages.get(first)?.remove();
        messages.delete(first);
      }
    }
    return node;
  }
  function receive(frame: unknown) {
    if (typeof frame !== "string" || frame.length > 262144)
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
      const value = data as Record<string, unknown>;
      if (event === "stream.lagged") {
        status.textContent = "消息有遗漏，历史不完整。";
        continue;
      }
      if (event === "conversation.typing") {
        status.textContent = value.typing ? "对方正在输入…" : "已连接";
        continue;
      }
      if (typeof value.message_id !== "string") continue;
      const id = `remote:${value.message_id}`;
      if (event === "message.delete") {
        messages.get(id)?.remove();
        messages.delete(id);
        continue;
      }
      const node = message(id);
      if (event === "message.delta" && typeof value.delta === "string")
        node.textContent = (node.textContent + value.delta).slice(-65536);
      if (event === "message.edit" && typeof value.text === "string")
        node.textContent = value.text.slice(-65536);
      if (event === "message.event" && value.type === "output_delta") {
        const payload = value.payload as Record<string, unknown> | null;
        if (payload && typeof payload.text === "string")
          node.textContent = (node.textContent + payload.text).slice(-65536);
      }
      if (event === "message.end" && value.error)
        node.append(element("small", "消息传输未完成"));
      if (
        [
          "message.file",
          "message.image",
          "message.audio",
          "message.video",
        ].includes(event ?? "") &&
        value.phase === "start"
      )
        node.textContent = "[收到附件，此页面暂不展示]";
    }
    history.scrollTop = history.scrollHeight;
  }
  function connect() {
    socket?.close();
    send.disabled = true;
    reconnect.disabled = true;
    status.textContent = "正在连接…";
    const url = new URL("/", document.baseURI);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    try {
      const connection = new WebSocket(url);
      socket = connection;
      const active = () => socket === connection && !lifecycle.signal.aborted;
      connection.addEventListener("open", () => {
        if (active()) {
          send.disabled = false;
          status.textContent = "已连接";
        }
      });
      connection.addEventListener("message", (event) => {
        if (!active()) return;
        try {
          receive(event.data);
        } catch {
          status.textContent = "收到无法识别的消息。";
        }
      });
      const disconnected = () => {
        if (active()) {
          send.disabled = true;
          reconnect.disabled = false;
          status.textContent = "连接已中断；未确认的消息不会自动重发。";
        }
      };
      connection.addEventListener("error", disconnected);
      connection.addEventListener("close", disconnected);
    } catch {
      reconnect.disabled = false;
      status.textContent = "无法建立连接。";
    }
  }
  form.addEventListener(
    "submit",
    (event) => {
      event.preventDefault();
      if (
        send.disabled ||
        socket?.readyState !== WebSocket.OPEN ||
        !input.value.trim()
      )
        return;
      if (new TextEncoder().encode(input.value).byteLength > 1024) {
        status.textContent = "消息超过 1024 字节，请缩短后发送。";
        return;
      }
      if (socket.bufferedAmount > 4096) {
        status.textContent = "发送队列繁忙，请稍后再试。";
        return;
      }
      try {
        socket.send(JSON.stringify({ text: input.value }));
        message(`local:${++localId}`).textContent =
          `你（已发送，未提供回执）：${input.value}`;
        input.value = "";
      } catch {
        status.textContent = "发送失败，请检查连接。";
      }
    },
    { signal: lifecycle.signal },
  );
  reconnect.addEventListener("click", connect, { signal: lifecycle.signal });
  const cleanup = () => {
    lifecycle.abort();
    socket?.close();
    messages.clear();
    input.value = "";
    page.remove();
    signal.removeEventListener("abort", cleanup);
  };
  signal.addEventListener("abort", cleanup, { once: true });
  connect();
  return cleanup;
}
