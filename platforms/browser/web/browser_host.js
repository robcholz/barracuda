const EVENT_STARTED = 1;
const EVENT_DEVICE_URL = 2;
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
  const portalRequests = [];
  let activePortalRequest;
  let portalResponse = [];
  const socketEvents = [];
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
    portal_request_len: () =>
      activePortalRequest || portalRequests.length === 0
        ? 0
        : portalRequests[0].request.byteLength,
    portal_request_read(pointer, length) {
      if (activePortalRequest || portalRequests.length === 0) return -1;
      const next = portalRequests.shift();
      if (next.request.byteLength !== length) return -1;
      activePortalRequest = next;
      portalResponse = [];
      memoryBytes(pointer, length).set(new Uint8Array(next.request));
      return length;
    },
    portal_response_write(pointer, length) {
      if (!activePortalRequest) return -1;
      portalResponse.push(memoryBytes(pointer, length).slice());
      return length;
    },
    portal_response_finish() {
      if (!activePortalRequest) return -1;
      const length = portalResponse.reduce(
        (sum, chunk) => sum + chunk.length,
        0,
      );
      const response = new Uint8Array(length);
      let offset = 0;
      for (const chunk of portalResponse) {
        response.set(chunk, offset);
        offset += chunk.length;
      }
      activePortalRequest.port.postMessage(response.buffer, [response.buffer]);
      activePortalRequest = undefined;
      portalResponse = [];
      return 0;
    },
    socket_event_kind: () => socketEvents[0]?.kind ?? 0,
    socket_event_id: () => socketEvents[0]?.id ?? 0,
    socket_event_len: () => socketEvents[0]?.payload?.byteLength ?? 0,
    socket_event_read(pointer, length) {
      const event = socketEvents.shift();
      if (!event || (event.payload?.byteLength ?? 0) !== length) return -1;
      if (length !== 0)
        memoryBytes(pointer, length).set(new Uint8Array(event.payload));
      return length;
    },
    socket_opened(id) {
      return socketStatus(id, (port) => port.postMessage({ type: "open" }));
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
      });
    },
    post_event(kind, pointer, length) {
      const message = decoder.decode(memoryBytes(pointer, length));
      if (kind === EVENT_STARTED) self.postMessage({ type: "started" });
      else if (kind === EVENT_DEVICE_URL)
        self.postMessage({ type: "device-url", url: message });
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
    enqueuePortalRequest(request, port) {
      portalRequests.push({ request, port });
      if (instance) requireInstance().exports.barracuda_browser_poll();
    },
    openSocket(id, port) {
      if (socketPorts.size !== 0) {
        port.postMessage({
          type: "error",
          message: "page socket is already in use",
        });
        port.close();
        return;
      }
      socketPorts.set(id, port);
      socketEvents.push({ kind: 1, id });
      port.onmessage = ({ data }) => {
        if (data?.type === "send" && data.payload instanceof ArrayBuffer) {
          socketEvents.push({ kind: 2, id, payload: data.payload });
        } else if (data?.type === "close") {
          socketEvents.push({ kind: 3, id });
        }
      };
      port.start();
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
