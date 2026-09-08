const EVENT_STARTED = 1;
const EVENT_ERROR = 3;

export async function createBrowserHost({ systemImage }) {
  const root = await navigator.storage.getDirectory();
  const directory = await root.getDirectoryHandle("barracuda", {
    create: true,
  });
  const file = await directory.getFileHandle("board.flash", { create: true });
  const flash = await file.createSyncAccessHandle();
  const zero = performance.now();
  const decoder = new TextDecoder();
  let instance;
  let pollQueued = false;
  const httpRequests = [];
  let activeHttpRequest;
  let httpResponse = [];
  const socketOpens = [];
  const socketEvents = new Map();
  const socketPorts = new Map();

  const memoryBytes = (pointer, length) =>
    new Uint8Array(requireInstance().exports.memory.buffer, pointer, length);
  const status = (operation) => {
    try {
      operation();
      return 0;
    } catch (error) {
      console.error(error);
      return -1;
    }
  };
  const transferred = (operation) => {
    try {
      return operation();
    } catch (error) {
      console.error(error);
      return -1;
    }
  };

  const imports = {
    boot_image_len: () => systemImage.byteLength,
    boot_image_read(pointer, length) {
      if (length !== systemImage.byteLength) return -1;
      memoryBytes(pointer, length).set(new Uint8Array(systemImage));
      return length;
    },
    flash_size: () => transferred(() => flash.getSize()),
    flash_truncate: (size) => status(() => flash.truncate(size)),
    flash_read: (offset, pointer, length) =>
      transferred(() =>
        flash.read(memoryBytes(pointer, length), { at: offset }),
      ),
    flash_write: (offset, pointer, length) =>
      transferred(() =>
        flash.write(memoryBytes(pointer, length), { at: offset }),
      ),
    flash_flush: () => status(() => flash.flush()),
    flash_close: () => flash.close(),
    monotonic_micros: () => Math.max(0, performance.now() - zero) * 1000,
    schedule_alarm(delayMillis) {
      try {
        return setTimeout(
          () => requireInstance().exports.barracuda_browser_alarm(),
          delayMillis,
        );
      } catch (error) {
        console.error(error);
        return -1;
      }
    },
    clear_alarm: (token) => clearTimeout(token),
    schedule_executor_poll() {
      if (pollQueued) return;
      pollQueued = true;
      queueMicrotask(() => {
        pollQueued = false;
        requireInstance().exports.barracuda_browser_poll();
      });
    },
    random_fill(pointer, length) {
      return status(() => crypto.getRandomValues(memoryBytes(pointer, length)));
    },
    http_request_port: () => httpRequests[0]?.targetPort ?? 0,
    http_request_len: () =>
      activeHttpRequest || httpRequests.length === 0
        ? 0
        : httpRequests[0].request.byteLength,
    http_request_read(pointer, length) {
      if (activeHttpRequest || httpRequests.length === 0) return -1;
      const next = httpRequests.shift();
      if (next.request.byteLength !== length) return -1;
      activeHttpRequest = next;
      httpResponse = [];
      memoryBytes(pointer, length).set(new Uint8Array(next.request));
      return length;
    },
    http_response_write(pointer, length) {
      if (!activeHttpRequest) return -1;
      httpResponse.push(memoryBytes(pointer, length).slice());
      return length;
    },
    http_response_finish() {
      if (!activeHttpRequest) return -1;
      const length = httpResponse.reduce((sum, chunk) => sum + chunk.length, 0);
      const response = new Uint8Array(length);
      let offset = 0;
      for (const chunk of httpResponse) {
        response.set(chunk, offset);
        offset += chunk.length;
      }
      activeHttpRequest.port.postMessage(response.buffer, [response.buffer]);
      activeHttpRequest = undefined;
      httpResponse = [];
      return 0;
    },
    socket_open_id: () => socketOpens[0]?.id ?? 0,
    socket_open_port: () => socketOpens[0]?.port ?? 0,
    socket_open_len: () => socketOpens[0]?.handshake.byteLength ?? 0,
    socket_open_read(pointer, length) {
      const open = socketOpens.shift();
      if (!open || open.handshake.byteLength !== length) return -1;
      memoryBytes(pointer, length).set(new Uint8Array(open.handshake));
      return length;
    },
    socket_event_kind: (id) => socketEvents.get(id)?.[0]?.kind ?? 0,
    socket_event_len: (id) =>
      socketEvents.get(id)?.[0]?.payload?.byteLength ?? 0,
    socket_event_text: (id) => (socketEvents.get(id)?.[0]?.text ? 1 : 0),
    socket_event_read(id, pointer, length) {
      const events = socketEvents.get(id);
      const event = events?.shift();
      if (!event || (event.payload?.byteLength ?? 0) !== length) return -1;
      if (length !== 0)
        memoryBytes(pointer, length).set(new Uint8Array(event.payload));
      return length;
    },
    socket_opened(id, pointer, length) {
      const protocol = decoder.decode(memoryBytes(pointer, length));
      return socketStatus(id, (port) =>
        port.postMessage({ type: "open", protocol }),
      );
    },
    socket_message(id, pointer, length, text) {
      return socketStatus(id, (port) => {
        const payload = memoryBytes(pointer, length).slice().buffer;
        port.postMessage({ type: "message", payload, text: text !== 0 }, [
          payload,
        ]);
      });
    },
    socket_closed(id) {
      return socketStatus(id, (port) => {
        port.postMessage({ type: "close" });
        port.close();
        socketPorts.delete(id);
        socketEvents.delete(id);
      });
    },
    socket_error(id, pointer, length) {
      const message = decoder.decode(memoryBytes(pointer, length));
      return socketStatus(id, (port) =>
        port.postMessage({ type: "error", message }),
      );
    },
    post_event(kind, pointer, length) {
      const message = decoder.decode(memoryBytes(pointer, length));
      if (kind === EVENT_STARTED) self.postMessage({ type: "started" });
      else if (kind === EVENT_ERROR)
        self.postMessage({ type: "error", message });
    },
  };

  function requireInstance() {
    if (!instance)
      throw new Error("Barracuda WASI instance is not initialized");
    return instance;
  }

  return {
    imports,
    setInstance(value) {
      instance = value;
    },
    enqueueHttpRequest(targetPort, request, port) {
      httpRequests.push({ targetPort, request, port });
      if (instance) requireInstance().exports.barracuda_browser_poll();
    },
    openSocket(id, targetPort, handshake, port) {
      socketPorts.set(id, port);
      socketEvents.set(id, []);
      socketOpens.push({ id, port: targetPort, handshake });
      port.onmessage = ({ data }) => {
        if (data?.type === "send" && data.payload instanceof ArrayBuffer) {
          socketEvents.get(id)?.push({
            kind: 1,
            payload: data.payload,
            text: data.text === true,
          });
        } else if (data?.type === "close") {
          socketEvents.get(id)?.push({ kind: 2 });
        }
        if (instance) requireInstance().exports.barracuda_browser_poll();
      };
      port.start();
      if (instance) requireInstance().exports.barracuda_browser_poll();
    },
  };

  function socketStatus(id, operation) {
    const port = socketPorts.get(id);
    if (!port) return -1;
    try {
      operation(port);
      return 0;
    } catch (error) {
      console.error(error);
      return -1;
    }
  }
}
