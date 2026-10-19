// Builds app containers from their sources.
//
//   bun build-app.ts <source> <bundle>                   one app, drawn with the system fonts
//   bun build-app.ts --own-fonts --each <sources> <bundles>  every app in <sources>, with its own fonts,
//                                                         each into <bundles>/<id>.bapp
//
// A source is a directory with `app.json`, the entry program, and an
// optional `app.tsx` view with its `app.d.ts` contract and `pocket.json`. The
// source manifest's `view` holds at most the `shape` the view is drawn for.
// The bundle is the built container: the manifest with `view` filled in (the
// files, and the size from `pocket.json`'s fixed viewport), the program,
// `view.bvir`, `styles.bin`, and any font atlases. Paths
// are relative to the working directory, so a builtin app Plugin runs this
// script on its own `resources/app`.
//
// A MicroTS view compiles with the PocketJS compiler, which is not part of this
// repository: point POCKETJS_ROOT at a PocketJS checkout. The script copies each
// app into that checkout so its `@pocketjs/*` imports resolve, compiles the view,
// bakes the fonts it uses, and links the View IR into `view.bvir`.
//
// Builtin apps ship no atlases and draw with the system fonts. Example
// installable apps carry their own, as an app from elsewhere would: when its
// view or program shows characters beyond ASCII, an app bakes them from Noto
// Sans SC; point CJK_FONT_DIR at a directory with NotoSansSC-Regular.otf and
// NotoSansSC-Bold.otf (github.com/notofonts/noto-cjk, Sans/SubsetOTF/SC).
//
//   POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run examples --plugin app-manager
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const pocket = process.env.POCKETJS_ROOT;
if (!pocket) {
  console.error("Set POCKETJS_ROOT to a PocketJS checkout.");
  process.exit(1);
}
const args = process.argv.slice(2);
const flag = (name: string) => {
  const at = args.indexOf(name);
  if (at >= 0) args.splice(at, 1);
  return at >= 0;
};
const ownFonts = flag("--own-fonts");
const each = flag("--each");
if (args.length !== 2) {
  console.error("Usage: bun build-app.ts [--own-fonts] [--each] <source> <bundle>");
  process.exit(1);
}
const [from, to] = args.map((path) => resolve(path));
const apps = each
  ? readdirSync(from, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => ({ name: entry.name, source: join(from, entry.name), bundle: join(to, `${entry.name}.bapp`), ownFonts }))
  : [{ name: JSON.parse(readFileSync(join(from, "app.json"), "utf8")).id, source: from, bundle: to, ownFonts }];
// The system icon font, so apps with their own fonts can show system icons too.
const icons = resolve(import.meta.dir, "icons/barracuda-icons.otf");
// Font slots 24 px and larger: their size and whether they are bold.
const DISPLAY_SLOTS: Record<number, [number, boolean]> = {
  5: [24, false], 6: [36, false], 12: [24, true], 13: [36, true], 14: [54, false], 15: [54, true],
};
const { buildAot } = await import(join(pocket, "microts/compiler/aot-build.ts"));
const { bakeAtlases, bakeSlot } = await import(join(pocket, "framework/compiler/bake-font.ts"));
const opentypeJs = (await import(join(pocket, "node_modules/opentype.js"))).default;
const opentype = async (path: string) => opentypeJs.parse(await Bun.file(path).arrayBuffer());

const strip = (value: unknown): unknown =>
  Array.isArray(value)
    ? value.map(strip)
    : value && typeof value === "object"
      ? Object.fromEntries(
          Object.entries(value).filter(([key]) => key !== "loc" && key !== "file").map(([key, item]) => [key, strip(item)]),
        )
      : value;

for (const { name, source, bundle, ownFonts } of apps) {
  rmSync(bundle, { recursive: true, force: true });
  mkdirSync(bundle, { recursive: true });
  // The source manifest's `view` has only its shape; the build fills in the
  // rest once the view exists.
  const manifest = JSON.parse(readFileSync(join(source, "app.json"), "utf8"));
  const shape: string | undefined = manifest.view?.shape;
  delete manifest.view;
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
    let fonts: Array<{ slot: number; bytes: Uint8Array }> = [];
    if (ownFonts) {
      const sources = [readFileSync(join(source, "app.tsx"), "utf8"), readFileSync(join(source, entry), "utf8")].join("");
      // `\uE068` and `\u{E068}` escapes name system icons, drawn from the icon font.
      const escaped = [...sources.matchAll(/\\u\{?([0-9A-Fa-f]{4,5})\}?/g)].map((match) => String.fromCodePoint(parseInt(match[1], 16)));
      const shown = sources + escaped.join("");
      const extraChars = [...new Set([...shown].filter((char) => char.codePointAt(0)! > 126 && !/\s/.test(char)))].join("");
      const cjk = process.env.CJK_FONT_DIR;
      if (extraChars && !cjk) {
        console.error(`${name}: shows characters beyond ASCII; set CJK_FONT_DIR to the Noto Sans SC fonts.`);
        process.exit(1);
      }
      const faces = extraChars ? { regularTtf: join(cjk!, "NotoSansSC-Regular.otf"), boldTtf: join(cjk!, "NotoSansSC-Bold.otf") } : {};
      // Display sizes (24 px and up) show numbers and icons, not prose: they
      // hold digits, a little punctuation, and the icons, which keeps their
      // atlases small.
      const slots: number[] = program.styles.usedFontSlots;
      const large = slots.filter((slot) => slot in DISPLAY_SLOTS);
      const small = slots.filter((slot) => !(slot in DISPLAY_SLOTS));
      if (small.length) fonts = await bakeAtlases({ codepoints: [], extraChars, slots: small, fallbackTtfs: [icons], ...faces });
      const shownLarge = [..."0123456789 .,:%-+"].concat([...extraChars].filter((char) => char.codePointAt(0)! >= 0xe000 && char.codePointAt(0)! <= 0xf8ff));
      for (const slot of large) {
        const [px, bold] = DISPLAY_SLOTS[slot];
        const face = await opentype(bold ? (faces.boldTtf ?? join(pocket, "assets/fonts/Inter-Bold.ttf")) : (faces.regularTtf ?? join(pocket, "assets/fonts/Inter-Regular.ttf")));
        const codepoints = [...new Set(shownLarge.map((char) => char.codePointAt(0)!))].sort((a, b) => a - b);
        fonts.push(bakeSlot(face, slot, px, bold, codepoints, 1, [await opentype(icons)]));
      }
      fonts.sort((left, right) => left.slot - right.slot);
      for (const font of fonts) writeFileSync(join(bundle, `font-${font.slot}.bin`), font.bytes);
    }
    const link = Bun.spawnSync(
      ["cargo", "run", "--quiet", "--package", "barracuda-ui-runtime", "--features", "compiler", "--bin", "ui-link", "--", view, join(bundle, "view.bvir")],
      { stdout: "inherit", stderr: "inherit" },
    );
    if (link.exitCode !== 0) process.exit(link.exitCode ?? 1);
    manifest.view = { program: "view.bvir", styles: "styles.bin" };
    if (fonts.length) manifest.view.fonts = fonts.map((font) => `font-${font.slot}.bin`);
    const size = JSON.parse(readFileSync(join(source, "pocket.json"), "utf8")).app?.viewport?.fixed?.logical;
    if (size) manifest.view.size = size;
    if (shape) manifest.view.shape = shape;
    writeManifest();
    console.log(`${name}: view.bvir, styles.bin, ${fonts.length} font(s)`);
  } finally {
    rmSync(staged, { recursive: true, force: true });
    rmSync(work, { recursive: true, force: true });
  }
}
