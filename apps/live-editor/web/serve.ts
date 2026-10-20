// Serves dist/ on http://localhost:8790 for trying Live Editor locally:
//
//   bun apps/live-editor/web/build.ts && bun apps/live-editor/web/serve.ts
import { join, normalize } from "node:path";

const dist = join(import.meta.dir, "dist");
const port = Number(process.env.PORT ?? 8790);
Bun.serve({
  port,
  async fetch(request) {
    const path = normalize(
      decodeURIComponent(new URL(request.url).pathname),
    ).replace(/^(\.\.[/\\])+/, "");
    const file = Bun.file(
      join(dist, path.endsWith("/") ? `${path}index.html` : path),
    );
    return (await file.exists())
      ? new Response(file)
      : new Response("Not found", { status: 404 });
  },
});
console.log(`Live Editor on http://localhost:${port}/`);
