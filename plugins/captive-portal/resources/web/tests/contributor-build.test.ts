import { expect, test } from "bun:test";
import { existsSync } from "node:fs";

const plugins = [
  "wifi",
  "agent",
  "agent-websearch",
  "imessage-qq",
  "imessage-wechat",
  "imessage-bluebubble",
  "imessage-telegram",
  "imessage-inkbox",
  "imessage-web",
];

/**
 * KiB per page bundle. Web chat carries a conversation, a WebGL mark, a session rail and its own
 * streaming Markdown package (healing, rendering and syntax highlighting, about 29 KB), so it gets
 * more room than a settings page.
 */
const BUDGET: Record<string, number> = { "imessage-web": 80 };

async function fresh(entry: string) {
  const built = await Bun.build({
    entrypoints: [entry],
    target: "browser",
    format: "esm",
    minify: true,
  });
  expect(built.success).toBe(true);
  expect(built.outputs).toHaveLength(1);
  return built.outputs[0].text();
}

test("each contributor has fresh standalone bundles in its own resource directory", async () => {
  const shell = await Bun.file(
    new URL("../../../filesystem/resources/app.js", import.meta.url),
  ).text();
  expect(shell).not.toContain("/api/gateway/");
  expect(shell).not.toContain("/api/model-api");
  for (const id of plugins) {
    const directory = new URL(`../../../../${id}/`, import.meta.url);
    const source = (name: string) =>
      new URL(`resources/web/${name}`, directory).pathname;
    const output = (name: string) =>
      Bun.file(new URL(`filesystem/resources/${name}`, directory));
    const code = await fresh(source("entry.ts"));
    expect(code).toBe(await output("entry.js").text());
    expect(new TextEncoder().encode(code).byteLength).toBeLessThan(
      (BUDGET[id] ?? 40) * 1024,
    );
    expect(code).not.toMatch(/\bimport\s*(?:\(|\{|["'])/);
    expect(code).toContain("mount");
    // the kit is bundled in, the kernel never is
    expect(code).not.toContain("data-hairline-style");

    const figure = ["figure.ts", "figure.js"]
      .map(source)
      .find((path) => existsSync(path));
    if (figure) {
      const built = await fresh(figure);
      expect(built).toBe(await output("figure.js").text());
      expect(built).not.toContain("data-hairline-style");
      expect(built).not.toMatch(/\bimport\s*(?:\(|\{|["'])/);
    } else expect(await output("figure.js").exists()).toBe(false);
    for (const icon of ["icon.svg", "icon.png"]) {
      const from = Bun.file(source(icon));
      if (await from.exists())
        expect(await output(icon).bytes()).toEqual(await from.bytes());
      else expect(await output(icon).exists()).toBe(false);
    }
  }
});
