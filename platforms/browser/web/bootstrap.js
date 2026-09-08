export async function bootBarracuda({
  wasmUrl,
  wasmModuleUrl,
  systemImageUrl,
  gatewayUrl,
}) {
  const [wasm, image] = await Promise.all([
    fetch(wasmUrl)
      .then(requireOk)
      .then((response) => response.arrayBuffer()),
    fetch(systemImageUrl)
      .then(requireOk)
      .then((response) => response.arrayBuffer()),
  ]);
  const worker = new Worker(new URL("./worker.js", import.meta.url), {
    type: "module",
  });
  const started = new Promise((resolve, reject) => {
    const cleanup = () => {
      worker.removeEventListener("message", onMessage);
      worker.removeEventListener("error", onError);
    };
    const onMessage = ({ data }) => {
      if (data?.type === "started") {
        cleanup();
        resolve();
      } else if (data?.type === "error") {
        cleanup();
        reject(new Error(data.message));
      }
    };
    const onError = (error) => {
      cleanup();
      reject(error);
    };
    worker.addEventListener("message", onMessage);
    worker.addEventListener("error", onError);
  });
  worker.postMessage({ wasm, wasmModuleUrl, image, gatewayUrl }, [wasm, image]);
  await started;
  return worker;
}

function requireOk(response) {
  if (!response.ok) throw new Error(`download failed: ${response.status}`);
  return response;
}
