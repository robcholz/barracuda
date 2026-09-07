import { expect, test } from "bun:test";
import { Window } from "happy-dom";
import { mount } from "../entry";

test("chat uses SSE-framed WebSocket events, sends content and closes on unload", () => {
  const browser = new Window({ url: "http://localhost/portal/" });
  const previous = {
    document: globalThis.document,
    WebSocket: globalThis.WebSocket,
  };
  const sent: string[] = [];
  const sockets: Socket[] = [];
  class Socket extends EventTarget {
    static OPEN = 1;
    readyState = 1;
    constructor(public url: string | URL) {
      super();
      sockets.push(this);
    }
    send(text: string) {
      sent.push(text);
    }
    close() {
      this.readyState = 3;
    }
  }
  Object.assign(globalThis, { document: browser.document, WebSocket: Socket });
  const root = browser.document.createElement("div");
  const controller = new AbortController();
  try {
    mount(root as unknown as HTMLElement, { signal: controller.signal });
    expect(String(sockets.at(-1)!.url)).toBe("ws://localhost/");
    sockets.at(-1)!.dispatchEvent(new Event("open"));
    const input = root.querySelector("textarea")!;
    input.value = "hello";
    root
      .querySelector("form")!
      .dispatchEvent(new browser.Event("submit", { cancelable: true }));
    expect(JSON.parse(sent[0])).toEqual({ text: "hello" });
    const emit = (event: string, data: unknown) =>
      sockets.at(-1)!.dispatchEvent(
        new MessageEvent("message", {
          data: `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
        }),
      );
    emit("message.start", { message_id: "m1", kind: "text" });
    emit("message.delta", { message_id: "m1", delta: "<img src=x>" });
    expect(root.textContent).toContain("<img src=x>");
    expect(root.querySelector("img")).toBeNull();
    emit("message.event", {
      message_id: "m1",
      type: "output_delta",
      payload: { text: "answer" },
    });
    expect(root.textContent).toContain("<img src=x>answer");
    emit("message.edit", { message_id: "m1", text: "edited" });
    expect(root.textContent).toContain("edited");
    emit("message.delete", { message_id: "m1" });
    expect(root.textContent).not.toContain("edited");
    emit("stream.lagged", { missed: 3 });
    expect(root.textContent).toContain("历史不完整");
    sockets
      .at(-1)!
      .dispatchEvent(
        new MessageEvent("message", { data: "event: bad\ndata: {\n\n" }),
      );
    expect(root.textContent).toContain("无法识别");
    sockets.at(-1)!.dispatchEvent(new Event("close"));
    expect(root.textContent).toContain("连接已中断");
    root.querySelector("button")!.click();
    sockets.at(-1)!.dispatchEvent(new Event("open"));
    input.value = "字".repeat(400);
    root
      .querySelector("form")!
      .dispatchEvent(new browser.Event("submit", { cancelable: true }));
    expect(sent).toHaveLength(1);
    expect(root.textContent).toContain("超过 1024 字节");
    controller.abort();
    expect(sockets.at(-1)!.readyState).toBe(3);
    expect(root.childElementCount).toBe(0);
  } finally {
    Object.assign(globalThis, previous);
    void browser.happyDOM.close();
  }
});
