// Builds the system view in resources/view/ into filesystem/resources/view/.
//
// The view compiles with the PocketJS compiler, which is not part of this
// repository: point POCKETJS_ROOT at a PocketJS checkout. Its fonts are baked
// from Noto Sans SC: point CJK_FONT_DIR at a directory holding
// NotoSansSC-Regular.otf and NotoSansSC-Bold.otf (from
// github.com/notofonts/noto-cjk, Sans/SubsetOTF/SC). The text atlases hold
// ASCII and every other character in the view, the shell Plugin's sources,
// and charset.txt, so text the shell builds at runtime is covered; display
// sizes (20 px and up) hold ASCII and display.txt only, which keeps the
// atlases small enough for the resources partition.
//
//   POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run view --plugin shell
import { cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const pocket = process.env.POCKETJS_ROOT;
const fonts = process.env.CJK_FONT_DIR;
if (!pocket || !fonts) {
  console.error("Set POCKETJS_ROOT to a PocketJS checkout and CJK_FONT_DIR to the Noto Sans SC fonts.");
  process.exit(1);
}
const plugin = resolve(import.meta.dir, "../..");
const source = join(plugin, "resources/view");
const output = join(plugin, "filesystem/resources/view");
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

// Every character outside ASCII that the view or the Plugin's code may show.
const withoutComments = (text: string) => text.split("\n").filter((line) => !line.startsWith("#")).join("\n");
const texts: string[] = [readFileSync(join(source, "app.tsx"), "utf8"), withoutComments(readFileSync(join(source, "charset.txt"), "utf8"))];
const collect = (dir: string) => {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) collect(path);
    else if (entry.name.endsWith(".rs")) texts.push(readFileSync(path, "utf8"));
  }
};
collect(join(plugin, "crates"));
const beyondAscii = (text: string) => [...new Set([...text].filter((char) => char.codePointAt(0)! > 126 && !/\s/.test(char)))].join("");
const extraChars = beyondAscii(texts.join(""));
const displayChars = beyondAscii(withoutComments(readFileSync(join(source, "display.txt"), "utf8")));
// Slots at 20 px and up: 4 and 5 regular, 11 and 12 bold, and the 54 px pair.
const DISPLAY_SLOTS = new Set([4, 5, 6, 11, 12, 13, 14, 15]);

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
  const faces = { regularTtf: join(fonts, "NotoSansSC-Regular.otf"), boldTtf: join(fonts, "NotoSansSC-Bold.otf") };
  const used: number[] = program.styles.usedFontSlots;
  const atlases = [
    ...(await bakeAtlases({ codepoints: [], extraChars, slots: used.filter((slot) => !DISPLAY_SLOTS.has(slot)), ...faces })),
    ...(await bakeAtlases({ codepoints: [], extraChars: displayChars, slots: used.filter((slot) => DISPLAY_SLOTS.has(slot)), ...faces })),
  ];
  for (const font of atlases) writeFileSync(join(output, `font-${font.slot}.bin`), font.bytes);
  const link = Bun.spawnSync(
    ["cargo", "run", "--quiet", "--package", "barracuda-ui-runtime", "--features", "compiler", "--bin", "ui-link", "--", view, join(output, "view.bvir")],
    { stdout: "inherit", stderr: "inherit" },
  );
  if (link.exitCode !== 0) process.exit(link.exitCode ?? 1);
  const slots = atlases.map((font: { slot: number }) => font.slot).sort((a: number, b: number) => a - b);
  console.log(`shell view: view.bvir, styles.bin, fonts ${slots.join(", ")}, ${extraChars.length} characters beyond ASCII`);
} finally {
  rmSync(staged, { recursive: true, force: true });
  rmSync(work, { recursive: true, force: true });
}
