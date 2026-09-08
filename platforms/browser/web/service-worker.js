import { parseGatewayUrl } from "./gateway.js";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
let bridgeClientId;
const clientTargets = new Map();

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) =>
  event.waitUntil(self.clients.claim()),
);

self.addEventListener("message", (event) => {
  if (event.data?.type === "register-bridge" && event.source?.id) {
    bridgeClientId = event.source.id;
  } else if (event.data?.type === "socket-open" && event.ports.length === 1) {
    event.waitUntil(forwardSocket(event.data, event.ports[0]));
  }
});

async function forwardSocket(open, port) {
  const client = await findClient();
  if (!client) {
    port.postMessage({ type: "error", message: "Barracuda page is not open" });
    return;
  }
  client.postMessage(
    {
      type: "socket-open",
      id: open.id,
      port: open.port,
      handshake: open.handshake,
    },
    [open.handshake, port],
  );
}

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (isPlatformAsset(url)) return;
  const explicit = parseGatewayUrl(url, self.registration.scope);
  const target = explicit ?? clientTargets.get(event.clientId);
  if (!target) return;
  if (event.resultingClientId)
    clientTargets.set(event.resultingClientId, target);
  event.respondWith(forwardToSystem(event, target, explicit !== undefined));
});

function isPlatformAsset(url) {
  const script = new URL("browser_websocket.js", self.registration.scope);
  return url.origin === script.origin && url.pathname === script.pathname;
}

async function forwardToSystem(event, target, stripGatewayPrefix) {
  const client = await findClient();
  if (!client)
    return new Response("Barracuda page is not open", { status: 503 });

  const request = await serializeRequest(
    event.request,
    target,
    stripGatewayPrefix,
  );
  const channel = new MessageChannel();
  const response = new Promise((resolve, reject) => {
    const timeout = setTimeout(
      () => reject(new Error("Barracuda System request timed out")),
      30_000,
    );
    channel.port1.onmessage = ({ data }) => {
      clearTimeout(timeout);
      if (data?.error) reject(new Error(data.error));
      else resolve(data);
    };
  });
  client.postMessage({ type: "http-request", port: target.port, request }, [
    request,
    channel.port2,
  ]);
  try {
    return parseResponse(await response, target.port);
  } catch (error) {
    return new Response(
      error instanceof Error ? error.message : String(error),
      {
        status: 502,
      },
    );
  }
}

async function findClient() {
  if (bridgeClientId) {
    const client = await self.clients.get(bridgeClientId);
    if (client) return client;
    bridgeClientId = undefined;
  }
  const clients = await self.clients.matchAll({
    type: "window",
    includeUncontrolled: true,
  });
  const scope = new URL(self.registration.scope);
  const bridge = clients.find((client) => {
    const url = new URL(client.url);
    return (
      url.origin === scope.origin &&
      (url.pathname === scope.pathname ||
        url.pathname === `${scope.pathname}index.html`)
    );
  });
  if (bridge) bridgeClientId = bridge.id;
  return bridge;
}

async function serializeRequest(request, target, stripGatewayPrefix) {
  const url = new URL(request.url);
  const systemPath = stripGatewayPrefix ? target.pathname : url.pathname;
  const headers = [];
  for (const [name, value] of request.headers) {
    const lower = name.toLowerCase();
    if (!["host", "connection", "content-length"].includes(lower)) {
      headers.push(`${name}: ${value}`);
    }
  }
  const body = ["GET", "HEAD"].includes(request.method)
    ? new Uint8Array()
    : new Uint8Array(await request.arrayBuffer());
  headers.push("Host: browser.barracuda");
  headers.push("Connection: close");
  if (body.length !== 0) headers.push(`Content-Length: ${body.length}`);
  const head = encoder.encode(
    `${request.method} ${systemPath}${url.search} HTTP/1.1\r\n${headers.join("\r\n")}\r\n\r\n`,
  );
  const result = new Uint8Array(head.length + body.length);
  result.set(head);
  result.set(body, head.length);
  return result.buffer;
}

function parseResponse(buffer, port) {
  const bytes = new Uint8Array(buffer);
  const split = findSequence(bytes, [13, 10, 13, 10]);
  if (split < 0) throw new Error("System returned an invalid HTTP response");
  const lines = decoder.decode(bytes.subarray(0, split)).split("\r\n");
  const match = /^HTTP\/\d(?:\.\d)? (\d{3})(?: (.*))?$/.exec(lines.shift());
  if (!match) throw new Error("System returned an invalid HTTP status");
  const headers = new Headers();
  let chunked = false;
  for (const line of lines) {
    const colon = line.indexOf(":");
    if (colon < 1) continue;
    const name = line.slice(0, colon).trim();
    const value = line.slice(colon + 1).trim();
    const lower = name.toLowerCase();
    if (lower === "transfer-encoding" && value.toLowerCase() === "chunked") {
      chunked = true;
    } else if (!["connection", "content-length"].includes(lower)) {
      headers.append(name, value);
    }
  }
  let body = bytes.slice(split + 4);
  if (chunked) body = decodeChunks(body);
  if (headers.get("content-type")?.includes("text/html")) {
    const script = new URL("browser_websocket.js", self.registration.scope);
    const html = decoder
      .decode(body)
      .replace(
        "</head>",
        `<script src="${script.pathname}" data-barracuda-port="${port}"></script></head>`,
      );
    body = encoder.encode(html);
  }
  return new Response(body, {
    status: Number(match[1]),
    statusText: match[2] || "",
    headers,
  });
}

function decodeChunks(bytes) {
  const chunks = [];
  let total = 0;
  let offset = 0;
  while (offset < bytes.length) {
    const lineEnd = findSequence(bytes, [13, 10], offset);
    if (lineEnd < 0) throw new Error("System returned invalid chunked HTTP");
    const size = Number.parseInt(
      decoder.decode(bytes.subarray(offset, lineEnd)).split(";", 1)[0],
      16,
    );
    if (!Number.isFinite(size))
      throw new Error("System returned invalid chunk size");
    if (size === 0) break;
    const start = lineEnd + 2;
    const end = start + size;
    if (end + 2 > bytes.length)
      throw new Error("System returned a truncated HTTP chunk");
    chunks.push(bytes.subarray(start, end));
    total += size;
    offset = end + 2;
  }
  const body = new Uint8Array(total);
  let output = 0;
  for (const chunk of chunks) {
    body.set(chunk, output);
    output += chunk.length;
  }
  return body;
}

function findSequence(bytes, sequence, start = 0) {
  outer: for (
    let index = start;
    index <= bytes.length - sequence.length;
    index += 1
  ) {
    for (let offset = 0; offset < sequence.length; offset += 1) {
      if (bytes[index + offset] !== sequence[offset]) continue outer;
    }
    return index;
  }
  return -1;
}
