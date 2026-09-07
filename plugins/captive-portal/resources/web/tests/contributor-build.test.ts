import { expect, test } from "bun:test";

const plugins = [
  "agent",
  "agent-websearch",
  "imessage-qq",
  "imessage-wechat",
  "imessage-bluebubble",
  "imessage-telegram",
  "imessage-inkbox",
  "imessage-web",
];

test("each contributor has one fresh standalone bundle in its own resource directory", async () => {
  const shell = await Bun.file(
    new URL("../../../filesystem/resources/app.js", import.meta.url),
  ).text();
  for (const id of plugins) {
    const directory = new URL(`../../../../${id}/`, import.meta.url);
    const built = await Bun.build({
      entrypoints: [new URL("resources/web/entry.ts", directory).pathname],
      target: "browser",
      format: "esm",
      minify: true,
    });
    expect(built.success).toBe(true);
    expect(built.outputs).toHaveLength(1);
    const code = await built.outputs[0].text();
    expect(code).toBe(
      await Bun.file(
        new URL("filesystem/resources/entry.js", directory),
      ).text(),
    );
    expect(new TextEncoder().encode(code).byteLength).toBeLessThan(8192);
    expect(code).not.toMatch(/\bimport\s*(?:\(|\{|["'])/);
    expect(shell).not.toContain(`/api/gateway/`);
    expect(shell).not.toContain("/api/model-api");
    expect(code).toContain("mount");
  }
});
