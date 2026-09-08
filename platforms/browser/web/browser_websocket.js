(() => {
  let nextSocketId = 1;

  class PageLocalWebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    CONNECTING = 0;
    OPEN = 1;
    CLOSING = 2;
    CLOSED = 3;
    readyState = PageLocalWebSocket.CONNECTING;
    bufferedAmount = 0;
    binaryType = "blob";
    extensions = "";
    protocol = "";
    onopen = null;
    onmessage = null;
    onerror = null;
    onclose = null;

    constructor(url) {
      super();
      this.url = new URL(url, document.baseURI).href;
      const parsed = new URL(this.url);
      if (
        !["ws:", "wss:"].includes(parsed.protocol) ||
        parsed.host !== location.host ||
        parsed.pathname !== "/"
      ) {
        throw new DOMException(
          "Only the Barracuda page-local WebSocket is available",
          "SecurityError",
        );
      }
      const controller = navigator.serviceWorker.controller;
      if (!controller)
        throw new DOMException(
          "Barracuda page bridge is not ready",
          "InvalidStateError",
        );
      const id = nextSocketId++;
      const channel = new MessageChannel();
      this.port = channel.port1;
      this.port.onmessage = ({ data }) => this.receive(data);
      this.port.start();
      controller.postMessage({ type: "socket-open", id }, [channel.port2]);
    }

    send(data) {
      if (this.readyState !== PageLocalWebSocket.OPEN) {
        throw new DOMException("WebSocket is not open", "InvalidStateError");
      }
      if (typeof data !== "string") {
        throw new TypeError(
          "Barracuda page-local WebSocket currently accepts text messages",
        );
      }
      const payload = new TextEncoder().encode(data).buffer;
      this.bufferedAmount += payload.byteLength;
      this.port.postMessage({ type: "send", payload }, [payload]);
      this.bufferedAmount = 0;
    }

    close() {
      if (this.readyState >= PageLocalWebSocket.CLOSING) return;
      this.readyState = PageLocalWebSocket.CLOSING;
      this.port.postMessage({ type: "close" });
    }

    receive(data) {
      if (data?.type === "open") {
        this.readyState = PageLocalWebSocket.OPEN;
        this.emit("open", new Event("open"));
      } else if (data?.type === "message") {
        const value = data.text
          ? new TextDecoder().decode(data.payload)
          : this.binaryType === "arraybuffer"
            ? data.payload
            : new Blob([data.payload]);
        this.emit("message", new MessageEvent("message", { data: value }));
      } else if (data?.type === "error") {
        this.emit("error", new Event("error"));
        this.finish();
      } else if (data?.type === "close") {
        this.finish();
      }
    }

    finish() {
      this.readyState = PageLocalWebSocket.CLOSED;
      this.emit("close", new CloseEvent("close"));
      this.port.close();
    }

    emit(type, event) {
      this.dispatchEvent(event);
      const handler = this[`on${type}`];
      if (typeof handler === "function") handler.call(this, event);
    }
  }

  Object.defineProperty(globalThis, "WebSocket", {
    configurable: true,
    writable: true,
    value: PageLocalWebSocket,
  });
})();
