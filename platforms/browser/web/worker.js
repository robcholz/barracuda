import * as wasi from "./wasi_snapshot_preview1.js";
import { createBrowserHost } from "./browser_host.js";

self.addEventListener("unhandledrejection", (event) => {
  const reason = event.reason;
  self.postMessage({
    type: "error",
    message: reason instanceof Error ? reason.stack : String(reason),
  });
});

self.addEventListener("error", (event) => {
  self.postMessage({
    type: "error",
    message: event.error instanceof Error ? event.error.stack : event.message,
  });
});

self.onmessage = async ({ data, ports }) => {
  if (data?.type === "portal-request") {
    if (!browserHost) {
      ports[0]?.postMessage({ error: "Barracuda System is not ready" });
      return;
    }
    browserHost.enqueuePortalRequest(data.request, ports[0]);
    return;
  }
  if (data?.type === "socket-open") {
    if (!browserHost) {
      ports[0]?.postMessage({
        type: "error",
        message: "Barracuda System is not ready",
      });
      return;
    }
    browserHost.openSocket(data.id, ports[0]);
    return;
  }
  const { wasm, image } = data;
  try {
    browserHost = await createBrowserHost({ systemImage: image });
    const { instance } = await WebAssembly.instantiate(wasm, {
      wasi_snapshot_preview1: wasi,
      barracuda_browser: browserHost.imports,
    });
    browserHost.setInstance(instance);
    wasi.setMemory(instance.exports.memory);
    const start = instance.exports.barracuda_browser_start;
    if (typeof start !== "function") {
      throw new Error(
        "Barracuda WASI module does not export its Browser entry",
      );
    }
    start();
  } catch (error) {
    self.postMessage({
      type: "error",
      message: error instanceof Error ? error.message : String(error),
    });
  }
};

let browserHost;
