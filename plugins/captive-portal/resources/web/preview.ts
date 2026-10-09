import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

/**
 * Local preview of the shell. The manifest lists the nine built-in contributors as they register
 * on a device; their assets (entry.js, figure.js, icons) are served from each plugin's own build
 * output when it exists and are 404 otherwise. No device API is served: business endpoints 404,
 * so pages show what they show when the device does not answer.
 *
 * PORT                 the port (default: any free one)
 * PORTAL_MANIFEST      "empty" serves an empty manifest (the empty state)
 * PORTAL_PREVIEW_ASSETS a directory of `<id>/<path>` files consulted before the plugins' outputs
 *
 * GET /__preview/drop/<id> removes an entry from the manifest; /__preview/reset restores them all.
 */
const shell = fileURLToPath(
  new URL("../../filesystem/resources/", import.meta.url),
);
const plugins = fileURLToPath(new URL("../../../", import.meta.url));

const text = (zh: string, en: string) => ({ zh, en });
const ENTRIES = [
  {
    id: "wifi",
    group: "device",
    order: 10,
    title: text("Wi-Fi", "Wi-Fi"),
    summary: text(
      "扫描、连接或忘记无线网络",
      "Scan, join or forget wireless networks",
    ),
    icon: "icon.svg",
    figure: "figure.js",
  },
  {
    id: "agent",
    group: "agent",
    order: 10,
    title: text("模型配置", "Models"),
    summary: text(
      "为主 Agent、子 Agent、记忆与压缩注册模型",
      "Register models for the main agent, sub-agents, memory and compaction",
    ),
    icon: "icon.svg",
    figure: "figure.js",
  },
  {
    id: "agent-websearch",
    group: "agent",
    order: 20,
    title: text("网页搜索", "Web search"),
    summary: text(
      "让 Agent 通过 Tavily 搜索网页",
      "Let the agent search the web with Tavily",
    ),
    icon: "icon.svg",
    figure: "figure.js",
  },
  {
    id: "imessage-web",
    group: "channel",
    order: 10,
    title: text("Web 聊天", "Web chat"),
    summary: text(
      "在浏览器里直接和设备对话",
      "Talk to the device right in the browser",
    ),
    icon: "icon.svg",
    figure: "figure.js",
  },
  {
    id: "imessage-telegram",
    group: "channel",
    order: 20,
    title: text("Telegram", "Telegram"),
    summary: text("通过 Bot 收发消息", "Send and receive through a bot"),
    icon: "icon.svg",
    figure: null,
  },
  {
    id: "imessage-wechat",
    group: "channel",
    order: 30,
    title: text("微信", "WeChat"),
    summary: text("微信消息通道", "WeChat message channel"),
    icon: "icon.svg",
    figure: null,
  },
  {
    id: "imessage-qq",
    group: "channel",
    order: 40,
    title: text("QQ", "QQ"),
    summary: text("QQ Bot 消息通道", "QQ bot message channel"),
    icon: "icon.svg",
    figure: null,
  },
  {
    id: "imessage-bluebubble",
    group: "channel",
    order: 50,
    title: text("BlueBubbles", "BlueBubbles"),
    summary: text(
      "经 BlueBubbles 接入 iMessage",
      "iMessage through BlueBubbles",
    ),
    icon: "icon.svg",
    figure: null,
  },
  {
    id: "imessage-inkbox",
    group: "channel",
    order: 60,
    title: text("Inkbox", "Inkbox"),
    summary: text("Inkbox 身份与邮件服务", "Inkbox identity and mail"),
    icon: "icon.png",
    figure: null,
  },
].map((entry) => {
  const asset = (path: string | null) =>
    path === null ? null : `/portal/assets/${entry.id}/${path}`;
  return {
    ...entry,
    icon: asset(entry.icon),
    figure: asset(entry.figure),
    module: asset("entry.js"),
  };
});

const dropped = new Set<string>();
const SEGMENT = /^[a-zA-Z0-9_.-]+$/;
const TYPES: Record<string, string> = {
  js: "text/javascript",
  css: "text/css",
  html: "text/html; charset=utf-8",
  svg: "image/svg+xml",
  png: "image/png",
};
const noStore = { "Cache-Control": "no-store" };

function file(path: string) {
  if (!existsSync(path)) return new Response("Not found", { status: 404 });
  const type = TYPES[path.split(".").pop() ?? ""] ?? "application/octet-stream";
  return new Response(Bun.file(path), {
    headers: { ...noStore, "Content-Type": type },
  });
}

const server = Bun.serve({
  hostname: "127.0.0.1",
  port: Number(process.env.PORT ?? 0),
  fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/")
      return Response.redirect(new URL("/portal/", request.url));
    if (path === "/portal/entries.json")
      return Response.json(
        process.env.PORTAL_MANIFEST === "empty"
          ? []
          : ENTRIES.filter((entry) => !dropped.has(entry.id)),
        { headers: noStore },
      );
    if (path.startsWith("/__preview/drop/")) {
      dropped.add(path.slice("/__preview/drop/".length));
      return new Response(null, { status: 204 });
    }
    if (path === "/__preview/reset") {
      dropped.clear();
      return new Response(null, { status: 204 });
    }
    const asset = /^\/portal\/assets\/([a-z0-9-]+)\/(.+)$/.exec(path);
    if (asset) {
      const [, id, relative] = asset;
      if (
        dropped.has(id) ||
        !ENTRIES.some((entry) => entry.id === id) ||
        !relative
          .split("/")
          .every((part) => SEGMENT.test(part) && part !== "." && part !== "..")
      )
        return new Response("Not found", { status: 404 });
      const override = process.env.PORTAL_PREVIEW_ASSETS;
      if (override && existsSync(resolve(override, id, relative)))
        return file(resolve(override, id, relative));
      return file(resolve(plugins, id, "filesystem/resources", relative));
    }
    const shellFiles: Record<string, string> = {
      "/portal/": "index.html",
      "/portal/app.js": "app.js",
      "/portal/app.css": "app.css",
    };
    if (shellFiles[path]) return file(resolve(shell, shellFiles[path]));
    return new Response("Not found", { status: 404 });
  },
});
console.log(
  `Portal preview: ${server.url}portal/ (${process.env.PORTAL_MANIFEST === "empty" ? "empty" : "nine-entry"} local manifest; no device API)`,
);
