import { resolve } from "node:path";

/** Build one plugin's entry; shared tooling never combines plugin outputs. */
export async function buildEntry(pluginRoot: string) {
  const result = await Bun.build({
    entrypoints: [resolve(pluginRoot, "resources/web/entry.ts")],
    outdir: resolve(pluginRoot, "filesystem/resources"),
    naming: "[name].[ext]",
    target: "browser",
    format: "esm",
    minify: true,
  });
  if (!result.success)
    throw new AggregateError(result.logs, "Plugin web build failed");
  for (const output of result.outputs)
    console.log(`${output.path}: ${output.size} bytes`);
}

if (import.meta.main) await buildEntry(process.cwd());
