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
    expect(
      await Bun.file(join(root, "filesystem/resources/figure.js")).exists(),
    ).toBe(false);
    await expect(buildEntry(join(root, "missing"))).rejects.toThrow();
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("optional figure and icon are built and copied, and removed with their sources", async () => {
  const root = await mkdtemp(join(tmpdir(), "plugin-web-"));
  const out = (name: string) =>
    Bun.file(join(root, "filesystem/resources", name));
  try {
    await Bun.write(
      join(root, "resources/web/entry.ts"),
      "export function mount() {}",
    );
    await Bun.write(
      join(root, "resources/web/figure.js"),
      "const { mk } = HL;\nexport const figure = { name: 'router', range: [0, 1, 2], mount: () => ({ destroy() { mk; } }) };",
    );
    await Bun.write(join(root, "resources/web/icon.svg"), "<svg/>");
    await buildEntry(root);
    const figure = await out("figure.js").text();
    expect(figure).toContain("HL");
    expect(figure).toContain("router");
    expect(figure).not.toMatch(/\bimport\s*(?:\(|\{|["'])/);
    expect(await out("icon.svg").text()).toBe("<svg/>");

    await rm(join(root, "resources/web/figure.js"));
    await rm(join(root, "resources/web/icon.svg"));
    await Bun.write(
      join(root, "resources/web/icon.png"),
      new Uint8Array([137, 80, 78, 71]),
    );
    await buildEntry(root);
    expect(await out("figure.js").exists()).toBe(false);
    expect(await out("icon.svg").exists()).toBe(false);
    expect((await out("icon.png").bytes()).length).toBe(4);

    // a figure that bundles the kernel is refused
    await Bun.write(
      join(root, "resources/web/figure.ts"),
      `import HL from ${JSON.stringify(join(import.meta.dir, "../../plugins/captive-portal/resources/web/src/live/kernel.js"))};\nexport const figure = { name: 'x', range: [0, 1, 2], mount: () => HL };`,
    );
    await expect(buildEntry(root)).rejects.toThrow(/kernel/);
    expect(await out("figure.js").exists()).toBe(false);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
