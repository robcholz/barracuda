// Bakes the system fonts into filesystem/resources/fonts/font-<slot>.bin.
//
// Views that ship no atlas for a font slot are drawn with these, so the
// system UI and the system apps share one set. Text is Noto Sans SC (point
// CJK_FONT_DIR at a directory holding NotoSansSC-Regular.otf and
// NotoSansSC-Bold.otf, from github.com/notofonts/noto-cjk,
// Sans/SubsetOTF/SC), monospace is the PocketJS checkout's JetBrains Mono
// (POCKETJS_ROOT), and icons come from ../icons/barracuda-icons.otf.
//
// Text slots hold ASCII and every character beyond ASCII in the sources that
// sources.txt names and in charset.txt; the 16 px regular and 18 px bold ones
// also hold every icon. Display slots hold little: 24 px bold ASCII and
// display.txt, 36 px the app icons, 54 px bold digits and the Agent mark. Monospace slots hold
// ASCII. Text with other characters shows a placeholder box for them.
//
//   POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run fonts --plugin ui
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

const pocket = process.env.POCKETJS_ROOT;
const fonts = process.env.CJK_FONT_DIR;
if (!pocket || !fonts) {
  console.error("Set POCKETJS_ROOT to a PocketJS checkout and CJK_FONT_DIR to the Noto Sans SC fonts.");
  process.exit(1);
}
const here = import.meta.dir;
const plugin = resolve(here, "../..");
const root = resolve(plugin, "../..");
const output = join(plugin, "filesystem/resources/fonts");
const { bakeSlot } = await import(join(pocket, "framework/compiler/bake-font.ts"));
const opentype = (await import(join(pocket, "node_modules/opentype.js"))).default;
const load = async (path: string) => opentype.parse(await Bun.file(path).arrayBuffer());

const lines = (file: string) =>
  readFileSync(join(here, file), "utf8")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"));

// Every file under a listed directory with a listed extension.
const sourceText: string[] = [];
const walk = (dir: string, extensions: string[]) => {
  if (!existsSync(dir)) return;
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path, extensions);
    else if (extensions.some((extension) => name.endsWith(extension))) sourceText.push(readFileSync(path, "utf8"));
  }
};
for (const line of lines("sources.txt")) {
  const [dir, ...extensions] = line.split(/\s+/);
  walk(join(root, dir), extensions);
}
sourceText.push(lines("charset.txt").join(""));

const codepoints = (text: string) => [...new Set([...text].map((char) => char.codePointAt(0)!))];
const ascii = Array.from({ length: 95 }, (_, index) => 32 + index);
const iconTable: Record<string, Array<{ codepoint: number }>> = JSON.parse(readFileSync(join(here, "../icons/icons.json"), "utf8"));
const icons = Object.values(iconTable).flat().map((layer) => layer.codepoint);
// App icons are the two-layer ones drawn on a tile; only they are drawn large.
const appIcons = Object.values(iconTable)
  .filter((layers) => layers.length > 1 || layers === iconTable.agent)
  .flat()
  .map((layer) => layer.codepoint);
const beyond = codepoints(sourceText.join("")).filter((cp) => cp > 126 && !(cp >= 0xe000 && cp <= 0xf8ff) && !/\s/.test(String.fromCodePoint(cp)));
const display = codepoints(lines("display.txt").join("")).filter((cp) => cp > 126);
const sorted = (set: number[]) => [...new Set(set)].sort((a, b) => a - b);

const regular = await load(join(fonts, "NotoSansSC-Regular.otf"));
const bold = await load(join(fonts, "NotoSansSC-Bold.otf"));
const mono = await load(join(pocket, "assets/fonts/JetBrainsMono-Regular.ttf"));
const iconFace = await load(join(here, "../icons/barracuda-icons.otf"));

// slot -> [face, px, bold, characters]
const text = sorted([...ascii, ...beyond]);
const withIcons = sorted([...text, ...icons]);
const digits = [..."0123456789.%-"].map((char) => char.codePointAt(0)!);
const slots: Array<[number, any, number, boolean, number[]]> = [
  [1, regular, 14, false, text],
  [2, regular, 16, false, withIcons],
  [8, bold, 14, true, text],
  [9, bold, 16, true, text],
  [10, bold, 18, true, withIcons],
  [12, bold, 24, true, sorted([...ascii, ...display])],
  [6, regular, 36, false, sorted(appIcons)],
  [15, bold, 54, true, sorted([...digits, ...iconTable.agent.map((layer) => layer.codepoint)])],
  [17, mono, 14, false, ascii],
  [18, mono, 16, false, ascii],
];

rmSync(output, { recursive: true, force: true });
mkdirSync(output, { recursive: true });
let total = 0;
for (const [slot, face, px, isBold, chars] of slots) {
  const atlas = bakeSlot(face, slot, px, isBold, chars, 1, [iconFace]);
  writeFileSync(join(output, `font-${slot}.bin`), atlas.bytes);
  total += atlas.bytes.length;
  console.log(`font-${slot}.bin: ${px} px${isBold ? " bold" : ""}, ${atlas.glyphCount} glyphs, ${atlas.bytes.length} bytes`);
}
console.log(`${beyond.length} characters beyond ASCII, ${icons.length} icon glyphs, ${total} bytes`);
