import { existsSync } from "node:fs";
import { rm } from "node:fs/promises";
import { resolve } from "node:path";

/** A string only the Hairline kernel contains: a figure bundle that has it bundled the kernel. */
const KERNEL_MARKER = "data-hairline-style";
const ICONS = ["icon.svg", "icon.png"] as const;

async function bundle(entry: string, outdir: string, name: string) {
  const result = await Bun.build({
    entrypoints: [entry],
    outdir,
    naming: `${name}.[ext]`,
    target: "browser",
    format: "esm",
    minify: true,
  });
  if (!result.success)
    throw new AggregateError(result.logs, `Plugin web build failed: ${entry}`);
  return result.outputs;
}

/**
 * Builds one plugin's web resources; shared tooling never combines plugin outputs.
 *
 * - `resources/web/entry.ts` → `filesystem/resources/entry.js` (required);
 * - `resources/web/figure.ts` or `figure.js` → `filesystem/resources/figure.js` (optional; it uses
 *   the shell's global `HL` and must not bundle the kernel);
 * - `resources/web/icon.svg` or `icon.png` → copied to `filesystem/resources/` (optional).
 *
 * An optional output whose source is gone is removed, so a stale figure or icon is never packaged.
 */
export async function buildEntry(pluginRoot: string) {
  const source = resolve(pluginRoot, "resources/web");
  const outdir = resolve(pluginRoot, "filesystem/resources");
  const outputs: { path: string; size: number }[] = [
    ...(await bundle(resolve(source, "entry.ts"), outdir, "entry")),
  ];

  const figure = ["figure.ts", "figure.js"]
    .map((name) => resolve(source, name))
    .filter((path) => existsSync(path));
  if (figure.length > 1)
    throw new Error(`${source} has both figure.ts and figure.js`);
  if (figure.length) {
    const [output] = await bundle(figure[0], outdir, "figure");
    if ((await Bun.file(output.path).text()).includes(KERNEL_MARKER)) {
      await rm(output.path, { force: true });
      throw new Error(
        `${figure[0]} bundles the Hairline kernel; use the shell's global HL instead`,
      );
    }
    outputs.push(output);
  } else await rm(resolve(outdir, "figure.js"), { force: true });

  for (const name of ICONS) {
    const from = Bun.file(resolve(source, name));
    const to = resolve(outdir, name);
    if (await from.exists()) {
      await Bun.write(to, from);
      outputs.push({ path: to, size: from.size });
    } else await rm(to, { force: true });
  }

  for (const output of outputs)
    console.log(`${output.path}: ${output.size} bytes`);
}

if (import.meta.main) await buildEntry(process.cwd());
