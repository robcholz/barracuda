self.onmessage = async ({ data: { wasm, wasmModuleUrl, image, gatewayUrl } }) => {
  const bindings = await import(wasmModuleUrl);
  await bindings.default(wasm);
  await bindings.start(gatewayUrl, new Uint8Array(image));
};
