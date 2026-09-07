import { fileURLToPath } from "node:url";
const root = fileURLToPath(
  new URL("../filesystem/resources/", import.meta.url),
);
// Local preview only: there are no simulated business modules in the firmware assets.
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: Number(process.env.PORT ?? 0),
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/")
      return Response.redirect(new URL("/portal/", request.url));
    if (path === "/portal/entries.json")
      return Response.json([], { headers: { "Cache-Control": "no-store" } });
    const files: Record<string, string> = {
      "/portal/": "index.html",
      "/portal/app.js": "app.js",
      "/portal/app.css": "app.css",
    };
    if (!files[path]) return new Response("Not found", { status: 404 });
    return new Response(Bun.file(`${root}/${files[path]}`), {
      headers: { "Cache-Control": "no-store" },
    });
  },
});
console.log(`Portal preview: ${server.url}portal/ (empty local manifest)`);
