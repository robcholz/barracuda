import { expect, test } from "bun:test";
import { existsSync } from "node:fs";

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

/**
 * Checks a portal contributor's committed bundles: `entry.js` (and `figure.js` when the plugin has a
 * figure) match a fresh build of its `resources/web` sources, import nothing, carry the portal kit but
 * never the shell's kernel, and the entry stays under `budgetKiB`; icons are copied unchanged.
 * `plugin` is the plugin's root directory.
 */
export function testContributorBundles(plugin: URL, budgetKiB = 40) {
  test("the page's bundles are fresh, standalone and within budget", async () => {
    const source = (name: string) =>
      new URL(`resources/web/${name}`, plugin).pathname;
    const output = (name: string) =>
      Bun.file(new URL(`filesystem/resources/${name}`, plugin));
    const code = await fresh(source("entry.ts"));
    expect(code).toBe(await output("entry.js").text());
    expect(new TextEncoder().encode(code).byteLength).toBeLessThan(
      budgetKiB * 1024,
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
  });
}
