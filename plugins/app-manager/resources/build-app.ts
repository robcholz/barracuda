// Builds app containers from their sources.
//
//   bun build-app.ts <source> <bundle>                   one app, drawn with the system fonts
//   bun build-app.ts --own-fonts --each <sources> <bundles>  every app in <sources>, with its own fonts,
//                                                         each into <bundles>/<id>.bapp
//   bun build-app.ts --target 360x360@1 <source> <bundle>  one app built for one display
//
// A source is a directory with `app.json`, the entry program, and an
// optional `app.tsx` view with its `app.d.ts` contract and `pocket.json`. The
// source manifest has no `view`; the build writes it. The bundle is the built
// container: the manifest with `view` filled in (the files, and the size from
// `pocket.json`'s fixed viewport), the program, `view.bvir`, `styles.bin`,
// and any font atlases. Paths
// are relative to the working directory, so a builtin app Plugin runs this
// script on its own `resources/app`.
//
// A MicroTS view compiles with the PocketJS compiler, which is not part of this
// repository: point POCKETJS_ROOT at a PocketJS checkout. The script stages a
// copy of the compiler with the patches in `pocketjs/` applied (the compiler
// half of the vendored core's patches), copies each app into it so its
// `@pocketjs/*` imports resolve, compiles the view, bakes the fonts it uses,
// and links the View IR into `view.bvir`. The checkout itself is left as it is.
//
// Without `--target`, the view is built for any display at least the size
// `pocket.json` names: the device lays it out at its own size. `--target
// <width>x<height>[@<density>]` builds it for one display, the one whose
// `app.device().display` reports that width, height and density: the build
// resolves the app's `pocket.json` against it into a plan, solves the view's
// `layout-baked` subtrees for that size in the core the device runs (the
// vendored `pocketjs-core`, soft wrap on, built for WebAssembly with the
// checkout's bindings), bakes the app's own fonts at that density,
// and records the display as the manifest's `view.target`, so only that
// display runs it.
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
import { stagePocketJS } from "./pocketjs/stage";

const checkout = process.env.POCKETJS_ROOT;
if (!checkout) {
  console.error("Set POCKETJS_ROOT to a PocketJS checkout.");
  process.exit(1);
}
// The compiler, staged with the patches in `pocketjs/`.
const pocket = stagePocketJS(checkout);
const args = process.argv.slice(2);
const flag = (name: string) => {
  const at = args.indexOf(name);
  if (at >= 0) args.splice(at, 1);
  return at >= 0;
};
const option = (name: string) => {
  const at = args.indexOf(name);
  return at >= 0 ? args.splice(at, 2)[1] : undefined;
};
const ownFonts = flag("--own-fonts");
const each = flag("--each");
const targetText = option("--target");
if (args.length !== 2) {
  console.error("Usage: bun build-app.ts [--own-fonts] [--each] [--target <width>x<height>[@<density>]] <source> <bundle>");
  process.exit(1);
}
// The display a targeted build is for: its size in view pixels and the
// display pixels each covers, as the manifest's `view.target` records it.
type Target = { size: [number, number]; density: number };
const target: Target | undefined = (() => {
  if (targetText === undefined) return undefined;
  const match = /^(\d+)x(\d+)(?:@([1-4]))?$/.exec(targetText);
  const [width, height] = [Number(match?.[1]), Number(match?.[2])];
  if (!match || !(width >= 1 && width <= 4096 && height >= 1 && height <= 4096)) {
    console.error(`--target ${targetText}: write <width>x<height>[@<density>], such as 360x360@1`);
    process.exit(1);
  }
  return { size: [width, height], density: Number(match[3] ?? 1) };
})();
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
const { analyzeAot, buildAot } = await import(join(pocket, "microts/compiler/aot-build.ts"));
const { planLayout } = await import(join(pocket, "microts/compiler/aot-layout-plan.ts"));
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

// Where an atlas names its font slot.
const FONT_SLOT_BYTE = 12;
// The system fonts views without atlases of their own draw with: a targeted
// build measures text with them.
const systemFonts = resolve(import.meta.dir, "../../system-fonts/filesystem/resources/fonts");
// The core the device runs, for WebAssembly: the checkout's wasm bindings
// (`engine/wasm`) over the vendored `pocketjs-core`, which carries
// Barracuda's patches, with soft wrap on as the UI runtime has it. The same crate
// and float math as the device, so the rects a targeted build solves, wrapped
// text included, are the ones the device would.
const vendoredCore = resolve(import.meta.dir, "../crates/pocketjs-core");
const coreWasm = () => {
  const bindings = join(checkout, "engine/wasm");
  const crate = resolve(import.meta.dir, "../../../target/pocketjs-wasm");
  const write = (path: string, text: string) => {
    if (!existsSync(path) || readFileSync(path, "utf8") !== text) writeFileSync(path, text);
  };
  const replace = (text: string, from: string, to: string, file: string) => {
    if (!text.includes(from)) {
      console.error(`${file} in POCKETJS_ROOT no longer has \`${from}\`; update build-app.ts for this PocketJS.`);
      process.exit(1);
    }
    return text.replace(from, to);
  };
  mkdirSync(join(crate, "src"), { recursive: true });
  const manifest = readFileSync(join(bindings, "Cargo.toml"), "utf8");
  write(join(crate, "Cargo.toml"), `${replace(manifest, `pocketjs-core = { path = "../core" }`, `pocketjs-core = { path = ${JSON.stringify(vendoredCore)} }`, "engine/wasm/Cargo.toml")}\n[workspace]\n`);
  if (existsSync(join(bindings, "Cargo.lock"))) write(join(crate, "Cargo.lock"), readFileSync(join(bindings, "Cargo.lock"), "utf8"));
  const init = "UI = Some(Ui::new_with_raster_density(raster_density.max(1)));";
  const lib = readFileSync(join(bindings, "src/lib.rs"), "utf8");
  write(join(crate, "src/lib.rs"), replace(lib, init, `${init}\n        ui().set_soft_wrap(true);`, "engine/wasm/src/lib.rs"));
  const built = Bun.spawnSync(["cargo", "build", "--release", "--target", "wasm32-unknown-unknown"], { cwd: crate, stdout: "inherit", stderr: "inherit" });
  if (built.exitCode !== 0) {
    console.error("pocketjs-wasm: cargo build failed (missing target? run: rustup target add wasm32-unknown-unknown)");
    process.exit(built.exitCode ?? 1);
  }
  return readFileSync(join(crate, "target/wasm32-unknown-unknown/release/pocketjs_wasm.wasm"));
};

/** Resolves the app's `pocket.json` against `target`: the view pixels its
 * view is laid out in and the density its atlases are baked at. */
function plan(name: string, pocketJson: { app?: { viewport?: { fixed?: { logical?: [number, number] } } } }, target: Target) {
  const least = pocketJson.app?.viewport?.fixed?.logical;
  if (least && (target.size[0] < least[0] || target.size[1] < least[1])) {
    console.error(`${name}: the view needs a display of at least ${least[0]}x${least[1]}, not ${target.size[0]}x${target.size[1]}`);
    process.exit(1);
  }
  return { viewport: target.size, density: target.density };
}

/** Marks each element of `nodes` with the rect `layout` solved for it, as
 * `ui-link` reads it. */
function annotate(nodes: any[], layout: { entries: WeakMap<object, unknown> }) {
  for (const node of nodes) {
    const entry = layout.entries.get(node);
    if (entry) node.layout = entry;
    for (const children of [node.children, ...(node.branches ?? []).map((branch: any) => branch.children)]) {
      if (children) annotate(children, layout);
    }
  }
}

for (const { name, source, bundle, ownFonts } of apps) {
  rmSync(bundle, { recursive: true, force: true });
  mkdirSync(bundle, { recursive: true });
  // The build fills in the manifest's `view` once the view exists.
  const manifest = JSON.parse(readFileSync(join(source, "app.json"), "utf8"));
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
    const pocketJson = JSON.parse(readFileSync(join(source, "pocket.json"), "utf8"));
    const resolved = target && plan(name, pocketJson, target);
    const density = resolved?.density ?? 1;
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
      const slots: number[] = analyzeAot(join(staged, "app.tsx"), { strict: true }).styles.usedFontSlots;
      const large = slots.filter((slot) => slot in DISPLAY_SLOTS);
      const small = slots.filter((slot) => !(slot in DISPLAY_SLOTS));
      if (small.length) fonts = await bakeAtlases({ codepoints: [], extraChars, slots: small, fallbackTtfs: [icons], rasterDensity: density, ...faces });
      const shownLarge = [..."0123456789 .,:%-+"].concat([...extraChars].filter((char) => char.codePointAt(0)! >= 0xe000 && char.codePointAt(0)! <= 0xf8ff));
      for (const slot of large) {
        const [px, bold] = DISPLAY_SLOTS[slot];
        const face = await opentype(bold ? (faces.boldTtf ?? join(checkout, "assets/fonts/Inter-Bold.ttf")) : (faces.regularTtf ?? join(checkout, "assets/fonts/Inter-Regular.ttf")));
        const codepoints = [...new Set(shownLarge.map((char) => char.codePointAt(0)!))].sort((a, b) => a - b);
        fonts.push(bakeSlot(face, slot, px, bold, codepoints, density, [await opentype(icons)]));
      }
      fonts.sort((left, right) => left.slot - right.slot);
      for (const font of fonts) writeFileSync(join(bundle, `font-${font.slot}.bin`), font.bytes);
    }
    // The atlases the device loads for the view: its own fill their slots,
    // the system fonts fill the rest, and as the UI runtime's `Document`
    // does, text without a size class (font slot 0) draws with the 16 px
    // regular default (slot 2) when no atlas is for slot 0.
    const atlases = () => {
      const own = new Set(fonts.map((font) => font.slot));
      const system = readdirSync(systemFonts)
        .filter((file) => /^font-\d+\.bin$/.test(file) && !own.has(Number(file.slice(5, -4))))
        .map((file) => new Uint8Array(readFileSync(join(systemFonts, file))));
      const loaded = [...fonts.map((font) => font.bytes), ...system];
      const slotOf = (atlas: Uint8Array) => atlas[FONT_SLOT_BYTE];
      const regular = loaded.find((atlas) => slotOf(atlas) === 2);
      if (regular && !loaded.some((atlas) => slotOf(atlas) === 0)) {
        const unstyled = regular.slice();
        unstyled[FONT_SLOT_BYTE] = 0;
        loaded.push(unstyled);
      }
      return loaded;
    };
    const layoutEnvironment = resolved && { viewport: resolved.viewport, fontAtlases: atlases(), wasm: coreWasm() };
    const result = await buildAot(join(staged, "app.tsx"), { outDir: work, strict: true, format: false, layoutEnvironment }).catch((error: unknown) => {
      // Rects solved at build time hold for one display only.
      if (!layoutEnvironment && String(error).includes("layout-baked: the build has no layout environment")) {
        throw new Error(`${name}: the view has a layout-baked subtree; build it for one display with --target <width>x<height>[@<density>]`, { cause: error });
      }
      throw error;
    });
    const program = result.program;
    // `ui-link` links the first component, the root.
    const root = program.components[0];
    if (layoutEnvironment) annotate(root.nodes, planLayout(program, root, root.nodes, layoutEnvironment));
    for (const region of result.layout.regions) console.log(`${name}: layout-baked ${region.name}: ${region.nodes} nodes, ${region.formulas} formula leaves`);
    const view = join(work, "view.json");
    writeFileSync(view, JSON.stringify(strip({ version: program.version, types: program.types, components: program.components })));
    cpSync(join(work, "styles.bin"), join(bundle, "styles.bin"));
    const link = Bun.spawnSync(
      ["cargo", "run", "--quiet", "--package", "barracuda-ui-runtime", "--features", "compiler", "--bin", "ui-link", "--", view, join(bundle, "view.bvir")],
      { stdout: "inherit", stderr: "inherit" },
    );
    if (link.exitCode !== 0) process.exit(link.exitCode ?? 1);
    manifest.view = { program: "view.bvir", styles: "styles.bin" };
    if (fonts.length) manifest.view.fonts = fonts.map((font) => `font-${font.slot}.bin`);
    const size = pocketJson.app?.viewport?.fixed?.logical;
    if (size) manifest.view.size = size;
    if (target) manifest.view.target = target;
    writeManifest();
    console.log(`${name}: view.bvir, styles.bin, ${fonts.length} font(s)${target ? `, for ${targetText}` : ""}`);
  } finally {
    rmSync(staged, { recursive: true, force: true });
    rmSync(work, { recursive: true, force: true });
  }
}
