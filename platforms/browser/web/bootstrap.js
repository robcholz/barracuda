export async function bootBarracuda({ wasmUrl, wasmModuleUrl, systemImageUrl, gatewayUrl }) {
  const [wasm, image] = await Promise.all([
    fetch(wasmUrl).then(requireOk).then((response) => response.arrayBuffer()),
    fetch(systemImageUrl).then(requireOk).then((response) => response.arrayBuffer()),
  ]);
  const worker = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  worker.postMessage({ wasm, wasmModuleUrl, image, gatewayUrl }, [wasm, image]);
  return worker;
}

function requireOk(response) {
  if (!response.ok) throw new Error(`download failed: ${response.status}`);
  return response;
}
