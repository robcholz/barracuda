import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL(".", import.meta.url));
const output = fileURLToPath(
  new URL("../filesystem/resources/", import.meta.url),
);
const result = await Bun.build({
  entrypoints: [`${root}src/app.ts`, `${root}src/app.css`],
  outdir: output,
  target: "browser",
  format: "esm",
  minify: true,
  naming: "[name].[ext]",
});
if (!result.success)
  throw new AggregateError(result.logs, "Portal build failed");
await Bun.write(`${output}/index.html`, Bun.file(`${root}src/index.html`));
const files = [...result.outputs, Bun.file(`${output}/index.html`)];
for (const file of files)
  console.log(`${"path" in file ? file.path : file.name}: ${file.size} bytes`);
console.log(`Total: ${files.reduce((sum, file) => sum + file.size, 0)} bytes`);
