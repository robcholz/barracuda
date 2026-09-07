import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildEntry } from "./build";

test("shared builder writes only the selected plugin's standalone module", async () => {
  const root = await mkdtemp(join(tmpdir(), "plugin-web-"));
  try {
    await Bun.write(
      join(root, "resources/web/entry.ts"),
      "export function mount() { return 'selected-plugin'; }",
    );
    await buildEntry(root);
    const output = await Bun.file(
      join(root, "filesystem/resources/entry.js"),
    ).text();
    expect(output).toContain("selected-plugin");
    expect(output).toContain("mount");
    expect(output).not.toContain("import(");
    await expect(buildEntry(join(root, "missing"))).rejects.toThrow();
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
