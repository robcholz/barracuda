export async function bootBarracuda({
  wasmUrl,
  systemImageUrl,
  onError = () => {},
}) {
  const serviceWorker = await installServiceWorker();
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
  worker.addEventListener("message", ({ data }) => {
    if (data?.type === "error") onError(data.message);
  });
  navigator.serviceWorker.addEventListener("message", (event) => {
    if (event.ports.length !== 1) return;
    if (event.data?.type === "http-request") {
      const request = event.data.request;
      worker.postMessage(
        { type: "http-request", port: event.data.port, request },
        [request, event.ports[0]],
      );
    } else if (event.data?.type === "socket-open") {
      const handshake = event.data.handshake;
      worker.postMessage(
        {
          type: "socket-open",
          id: event.data.id,
          port: event.data.port,
          handshake,
        },
        [handshake, event.ports[0]],
      );
    }
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
  worker.postMessage({ wasm, image }, [wasm, image]);
  await started;
  await serviceWorker;
  return worker;
}

async function installServiceWorker() {
  if (!("serviceWorker" in navigator)) {
    throw new Error(
      "this browser does not support the page-local network bridge",
    );
  }
  await navigator.serviceWorker.register(
    new URL("./service-worker.js", import.meta.url),
    { type: "module", scope: "./" },
  );
  const registration = await navigator.serviceWorker.ready;
  registration.active?.postMessage({ type: "register-bridge" });
  return registration;
}

function requireOk(response) {
  if (!response.ok) throw new Error(`download failed: ${response.status}`);
  return response;
}
