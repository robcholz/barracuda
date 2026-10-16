// Builds the system view in resources/view/ into filesystem/resources/view/.
//
// The view compiles with the PocketJS compiler, which is not part of this
// repository: point POCKETJS_ROOT at a PocketJS checkout. It ships no font
// atlases: it draws with the system fonts, whose build reads this view and
// the entry point's sources for the characters to hold.
//
//   POCKETJS_ROOT=~/src/pocketjs cargo plugin run view --plugin app-manager
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const pocket = process.env.POCKETJS_ROOT;
if (!pocket) {
  console.error("Set POCKETJS_ROOT to a PocketJS checkout.");
  process.exit(1);
}
const plugin = resolve(import.meta.dir, "../..");
const source = join(plugin, "resources/view");
const output = join(plugin, "filesystem/resources/view");
const { buildAot } = await import(join(pocket, "microts/compiler/aot-build.ts"));

const strip = (value: unknown): unknown =>
  Array.isArray(value)
    ? value.map(strip)
    : value && typeof value === "object"
      ? Object.fromEntries(
          Object.entries(value).filter(([key]) => key !== "loc" && key !== "file").map(([key, item]) => [key, strip(item)]),
        )
      : value;

rmSync(output, { recursive: true, force: true });
mkdirSync(output, { recursive: true });
const staged = join(pocket, "apps", ".barracuda-shell");
rmSync(staged, { recursive: true, force: true });
cpSync(source, staged, { recursive: true });
const work = mkdtempSync(join(tmpdir(), "barracuda-shell-"));
try {
  const result = await buildAot(join(staged, "app.tsx"), { outDir: work, strict: true, format: false });
  const program = result.program;
  const view = join(work, "view.json");
  writeFileSync(view, JSON.stringify(strip({ version: program.version, types: program.types, components: program.components })));
  cpSync(join(work, "styles.bin"), join(output, "styles.bin"));
  const link = Bun.spawnSync(
    ["cargo", "run", "--quiet", "--package", "barracuda-ui-runtime", "--features", "compiler", "--bin", "ui-link", "--", view, join(output, "view.bvir")],
    { stdout: "inherit", stderr: "inherit" },
  );
  if (link.exitCode !== 0) process.exit(link.exitCode ?? 1);
  console.log(`system view: view.bvir, styles.bin; font slots ${program.styles.usedFontSlots.join(", ")}`);
} finally {
  rmSync(staged, { recursive: true, force: true });
  rmSync(work, { recursive: true, force: true });
}
