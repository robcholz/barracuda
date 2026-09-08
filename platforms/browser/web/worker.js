self.onmessage = async ({
  data: { wasm, wasmModuleUrl, image, gatewayUrl },
}) => {
  try {
    const bindings = await import(wasmModuleUrl);
    await bindings.default(wasm);
    await bindings.start(gatewayUrl, new Uint8Array(image));
    self.postMessage({ type: "started" });
  } catch (error) {
    self.postMessage({
      type: "error",
      message: error instanceof Error ? error.message : String(error),
    });
  }
};
