(() => {
  const NativeWebSocket = globalThis.WebSocket;
  const targetPort = Number(document.currentScript?.dataset.barracudaPort);

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

    constructor(url, protocols = []) {
      super();
      this.url = new URL(url, document.baseURI).href;
      const parsed = new URL(this.url);
      if (!["ws:", "wss:"].includes(parsed.protocol)) {
        throw new DOMException("Invalid WebSocket URL", "SyntaxError");
      }
      if (parsed.host !== location.host) {
        return new NativeWebSocket(url, protocols);
      }
      if (
        !Number.isInteger(targetPort) ||
        targetPort < 1 ||
        targetPort > 65_535
      ) {
        throw new DOMException(
          "Barracuda System service target is unavailable",
          "InvalidStateError",
        );
      }
      const controller = navigator.serviceWorker.controller;
      if (!controller) {
        throw new DOMException(
          "Barracuda page bridge is not ready",
          "InvalidStateError",
        );
      }

      const requestedProtocols = normalizeProtocols(protocols);
      let id = 0;
      while (id === 0) id = crypto.getRandomValues(new Uint32Array(1))[0];
      const channel = new MessageChannel();
      const handshake = websocketHandshake(parsed, requestedProtocols);
      this.port = channel.port1;
      this.port.onmessage = ({ data }) => this.receive(data);
      this.port.start();
      controller.postMessage(
        { type: "socket-open", id, port: targetPort, handshake },
        [handshake, channel.port2],
      );
    }

    send(data) {
      if (this.readyState !== PageLocalWebSocket.OPEN) {
        throw new DOMException("WebSocket is not open", "InvalidStateError");
      }
      if (typeof data === "string") {
        this.sendPayload(new TextEncoder().encode(data).buffer, true);
      } else if (data instanceof ArrayBuffer) {
        this.sendPayload(data.slice(0), false);
      } else if (ArrayBuffer.isView(data)) {
        this.sendPayload(
          data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength),
          false,
        );
      } else if (data instanceof Blob) {
        data.arrayBuffer().then((payload) => this.sendPayload(payload, false));
      } else {
        throw new TypeError(
          "WebSocket data must be text, Blob, or binary data",
        );
      }
    }

    close() {
      if (this.readyState >= PageLocalWebSocket.CLOSING) return;
      this.readyState = PageLocalWebSocket.CLOSING;
      this.port.postMessage({ type: "close" });
    }

    sendPayload(payload, text) {
      this.bufferedAmount += payload.byteLength;
      this.port.postMessage({ type: "send", payload, text }, [payload]);
      this.bufferedAmount = 0;
    }

    receive(data) {
      if (data?.type === "open") {
        this.protocol = data.protocol ?? "";
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
        console.error(data.message);
        this.emit("error", new Event("error"));
      } else if (data?.type === "close") {
        this.finish();
      }
    }

    finish() {
      if (this.readyState === PageLocalWebSocket.CLOSED) return;
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

  function normalizeProtocols(protocols) {
    if (typeof protocols === "string") return [protocols];
    return Array.from(protocols);
  }

  function websocketHandshake(url, protocols) {
    const nonce = new Uint8Array(16);
    crypto.getRandomValues(nonce);
    const key = btoa(String.fromCharCode(...nonce));
    const path = `${url.pathname}${url.search}` || "/";
    const headers = [
      `GET ${path} HTTP/1.1`,
      `Host: browser.barracuda:${targetPort}`,
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Key: ${key}`,
      "Sec-WebSocket-Version: 13",
    ];
    if (protocols.length !== 0) {
      headers.push(`Sec-WebSocket-Protocol: ${protocols.join(", ")}`);
    }
    return new TextEncoder().encode(`${headers.join("\r\n")}\r\n\r\n`).buffer;
  }

  Object.defineProperty(globalThis, "WebSocket", {
    configurable: true,
    writable: true,
    value: PageLocalWebSocket,
  });
})();
