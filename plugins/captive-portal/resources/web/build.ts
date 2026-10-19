import { Glob } from "bun";
import { fileURLToPath } from "node:url";
import { prune, scan } from "../../../../ui/prune";

const root = fileURLToPath(new URL(".", import.meta.url));
const plugins = fileURLToPath(new URL("../../../", import.meta.url));
const output = fileURLToPath(
  new URL("../../filesystem/resources/", import.meta.url),
);

/**
 * What the portal draws: the shell, its UI kit and its own stylesheet (for the tokens its layout
 * reads), and every built-in page, since pages draw with the shell's stylesheet. Tests and build
 * scripts draw nothing.
 */
async function sources(): Promise<string[]> {
  const texts = [await Bun.file(`${root}src/app.css`).text()];
  for await (const path of new Glob("*/resources/web/**/*.{ts,js,html}").scan(
    plugins,
  ))
    if (!/\/tests\/|\/(build|preview)\.ts$/.test(path))
      texts.push(await Bun.file(plugins + path).text());
  return texts;
}

/** The stylesheets pages add while they are open (`style.ts` exports), which the shell leaves to them. */
async function pageStyles(): Promise<string[]> {
  const sheets: string[] = [];
  for await (const path of new Glob("*/resources/web/**/style.ts").scan(
    plugins,
  )) {
    if (path.startsWith("captive-portal/")) continue;
    const module: Record<string, unknown> = await import(plugins + path);
    for (const value of Object.values(module))
      if (typeof value === "string") sheets.push(value);
  }
  return sheets;
}

/** The repository's shared UI (ui/), not this shell's own kit in ./ui. */
const sharedUi = fileURLToPath(new URL("../../../../ui/", import.meta.url));
const names = ["tokens.css", "components.css"];
const cut = prune(
  await Promise.all(names.map((name) => Bun.file(sharedUi + name).text())),
  scan(await sources()),
  await pageStyles(),
);
const result = await Bun.build({
  entrypoints: [`${root}src/app.ts`, `${root}src/app.css`],
  outdir: output,
  target: "browser",
  format: "esm",
  minify: true,
  naming: "[name].[ext]",
  plugins: [
    {
      name: "shared-ui",
      setup(build) {
        build.onLoad(
          { filter: /[\\/]ui[\\/](tokens|components)\.css$/ },
          (args) => {
            const index = args.path.startsWith(sharedUi)
              ? names.indexOf(args.path.slice(sharedUi.length))
              : -1;
            if (index < 0)
              throw new Error(`${args.path} is not in ${sharedUi}`);
            return { contents: cut[index], loader: "css" };
          },
        );
      },
    },
  ],
});
if (!result.success)
  throw new AggregateError(result.logs, "Portal build failed");
await Bun.write(`${output}/index.html`, Bun.file(`${root}src/index.html`));
const files = [...result.outputs, Bun.file(`${output}/index.html`)];
for (const file of files)
  console.log(`${"path" in file ? file.path : file.name}: ${file.size} bytes`);
console.log(`Total: ${files.reduce((sum, file) => sum + file.size, 0)} bytes`);
