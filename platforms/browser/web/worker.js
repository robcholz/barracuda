import { setMemory as setWasiMemory } from "./wasi_snapshot_preview1.js";

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

self.onmessage = async ({
  data: { wasm, wasmModuleUrl, image, gatewayUrl },
}) => {
  try {
    const bindings = await import(wasmModuleUrl);
    const instance = await bindings.default(wasm);
    setWasiMemory(instance.memory);
    await bindings.start(gatewayUrl, new Uint8Array(image));
    self.postMessage({ type: "started" });
  } catch (error) {
    self.postMessage({
      type: "error",
      message: error instanceof Error ? error.message : String(error),
    });
  }
};
