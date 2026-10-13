// Builds every system app under resources/apps/<name>/ into the image's
// filesystem/resources/apps/<name>/, and every example installable app under
// resources/examples/<name>/ into crates/plugin/tests/fixtures/<name>/, a
// container ready to copy into the Workspace's media/apps/.
//
// A MicroTS view compiles with the PocketJS compiler, which is not part of this
// repository: point POCKETJS_ROOT at a PocketJS checkout. The script copies each
// app into that checkout so its `@pocketjs/*` imports resolve, compiles the view,
// bakes the fonts it uses, and links the View IR into `view.bvir`.
//
// An app whose view or program shows characters beyond ASCII bakes its fonts
// from Noto Sans SC instead of Inter: point CJK_FONT_DIR at a directory with
// NotoSansSC-Regular.otf and NotoSansSC-Bold.otf (github.com/notofonts/noto-cjk,
// Sans/SubsetOTF/SC). The atlases then hold every such character in the
// app's sources.
//
//   POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run apps --plugin app
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const pocket = process.env.POCKETJS_ROOT;
if (!pocket) {
  console.error("Set POCKETJS_ROOT to a PocketJS checkout.");
  process.exit(1);
}
const plugin = resolve(import.meta.dir, "../..");
const groups = [
  { sources: join(plugin, "resources/apps"), output: join(plugin, "filesystem/resources/apps") },
  { sources: join(plugin, "resources/examples"), output: join(plugin, "crates/plugin/tests/fixtures") },
];
const { buildAot } = await import(join(pocket, "microts/compiler/aot-build.ts"));
const { bakeAtlases } = await import(join(pocket, "framework/compiler/bake-font.ts"));

const strip = (value: unknown): unknown =>
  Array.isArray(value)
    ? value.map(strip)
    : value && typeof value === "object"
      ? Object.fromEntries(
          Object.entries(value).filter(([key]) => key !== "loc" && key !== "file").map(([key, item]) => [key, strip(item)]),
        )
      : value;

const apps = groups.flatMap(({ sources, output }) =>
  readdirSync(sources, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => ({ name: entry.name, source: join(sources, entry.name), bundle: join(output, entry.name) })),
);
for (const { name, source, bundle } of apps) {
  rmSync(bundle, { recursive: true, force: true });
  mkdirSync(bundle, { recursive: true });
  // The source manifest has no `view`; the build adds it once the view exists.
  const manifest = JSON.parse(readFileSync(join(source, "app.json"), "utf8"));
  const entry = manifest.entry ?? "main.lua";
  const writeManifest = () => writeFileSync(join(bundle, "app.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  cpSync(join(source, entry), join(bundle, entry));
  if (!existsSync(join(source, "app.tsx"))) {
    writeManifest();
    console.log(`${name}: headless`);
    continue;
  }
  const staged = join(pocket, "apps", `.barracuda-${name}`);
  rmSync(staged, { recursive: true, force: true });
  cpSync(source, staged, { recursive: true });
  const work = mkdtempSync(join(tmpdir(), `barracuda-app-${name}-`));
  try {
    const result = await buildAot(join(staged, "app.tsx"), { outDir: work, strict: true, format: false });
    const program = result.program;
    const view = join(work, "view.json");
    writeFileSync(view, JSON.stringify(strip({ version: program.version, types: program.types, components: program.components })));
    cpSync(join(work, "styles.bin"), join(bundle, "styles.bin"));
    const shown = [readFileSync(join(source, "app.tsx"), "utf8"), readFileSync(join(source, entry), "utf8")].join("");
    const extraChars = [...new Set([...shown].filter((char) => char.codePointAt(0)! > 126 && !/\s/.test(char)))].join("");
    const cjk = process.env.CJK_FONT_DIR;
    if (extraChars && !cjk) {
      console.error(`${name}: shows characters beyond ASCII; set CJK_FONT_DIR to the Noto Sans SC fonts.`);
      process.exit(1);
    }
    const faces = extraChars ? { regularTtf: join(cjk!, "NotoSansSC-Regular.otf"), boldTtf: join(cjk!, "NotoSansSC-Bold.otf") } : {};
    const fonts = await bakeAtlases({ codepoints: [], extraChars, slots: program.styles.usedFontSlots, ...faces });
    fonts.sort((left: { slot: number }, right: { slot: number }) => left.slot - right.slot);
    for (const font of fonts) writeFileSync(join(bundle, `font-${font.slot}.bin`), font.bytes);
    const link = Bun.spawnSync(
      ["cargo", "run", "--quiet", "--package", "barracuda-ui-runtime", "--features", "compiler", "--bin", "ui-link", "--", view, join(bundle, "view.bvir")],
      { stdout: "inherit", stderr: "inherit" },
    );
    if (link.exitCode !== 0) process.exit(link.exitCode ?? 1);
    manifest.view = {
      program: "view.bvir",
      styles: "styles.bin",
      fonts: fonts.map((font: { slot: number }) => `font-${font.slot}.bin`),
    };
    writeManifest();
    console.log(`${name}: view.bvir, styles.bin, ${fonts.length} font(s)`);
  } finally {
    rmSync(staged, { recursive: true, force: true });
    rmSync(work, { recursive: true, force: true });
  }
}
