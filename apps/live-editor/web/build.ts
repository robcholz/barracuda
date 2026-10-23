// Builds Live Editor into dist/, a static site:
//
//   index.html, main.js, app.css   the page (app.css carries the design system), and workers/ for
//                                  the build and run workers; assets/ for its figures
//   live-editor.wasm    the view linker and the preview (apps/live-editor/wasm)
//   lua.wasm            Lua 5.4 for the preview's runs (wasmoon)
//   fonts/              the device's system fonts (plugins/system-fonts)
//   templates.json      the app manager's example apps, for New app
//   microts.js          PocketJS's MicroTS compiler, bundled for the browser
//   microts-files.json  the files it reads (TypeScript's library declarations, PocketJS's sources)
//
// The MicroTS compiler is not part of this repository: point POCKETJS_ROOT at a PocketJS checkout,
// at the commit docs/plugins/app-manager/pocketjs.mdx names, as build-app.ts needs it; the build
// stages it with Barracuda's patches to it, as build-app.ts does.
//
//   POCKETJS_ROOT=~/src/pocketjs bun apps/live-editor/web/build.ts
//
// The compiler reads files as it compiles. To know which, this compiles every app in the
// repository on disk first and records each file it reads; an app that imports a PocketJS module
// none of them use would not find it in the browser.
import fs from "node:fs";
import { mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { stagePocketJS } from "../../../plugins/app-manager/resources/pocketjs/stage";

const web = import.meta.dir;
const repository = resolve(web, "../../..");
const dist = join(web, "dist");
mkdirSync(dist, { recursive: true });

// The linker and the preview, as WebAssembly. The crate is a workspace of its own, so it builds
// without a Board selected.
const targetDir = resolve(
  repository,
  process.env.CARGO_TARGET_DIR ?? "target/live-editor-wasm",
);
const cargo = Bun.spawnSync(
  [
    "cargo",
    "build",
    "--release",
    "--locked",
    "--target",
    "wasm32-unknown-unknown",
    "--manifest-path",
    join(repository, "apps/live-editor/wasm/Cargo.toml"),
    "--target-dir",
    targetDir,
  ],
  { cwd: repository, stdout: "inherit", stderr: "inherit" },
);
if (cargo.exitCode !== 0) process.exit(cargo.exitCode ?? 1);
fs.copyFileSync(
  join(
    targetDir,
    "wasm32-unknown-unknown/release/barracuda_live_editor_wasm.wasm",
  ),
  join(dist, "live-editor.wasm"),
);
console.log("live-editor.wasm");

await buildApp();

const checkout =
  process.env.POCKETJS_ROOT && resolve(process.env.POCKETJS_ROOT);
if (!checkout) {
  if (fs.existsSync(join(dist, "microts.js"))) {
    console.log("microts.js: kept (set POCKETJS_ROOT to rebuild it)");
    process.exit(0);
  }
  console.error(
    "Set POCKETJS_ROOT to a PocketJS checkout to bundle the MicroTS compiler.",
  );
  process.exit(1);
}
// The compiler, staged with Barracuda's patches to it, as build-app.ts stages it.
const pocket = stagePocketJS(checkout);

// Every app in the repository with a view: the system view, the builtin apps and the examples.
const apps = [
  join(repository, "plugins/app-manager/resources/view"),
  ...readdirSync(
    join(repository, "plugins/app-manager/resources/examples"),
  ).map((name) =>
    join(repository, "plugins/app-manager/resources/examples", name),
  ),
  ...readdirSync(join(repository, "plugins"))
    .map((name) => join(repository, "plugins", name, "resources/app"))
    .filter((path) => fs.existsSync(join(path, "app.tsx"))),
];

// Record every file the compiler reads while it compiles them, staged where build-app.ts stages them.
const read = new Set<string>();
const hooked = fs as unknown as Record<string, (...args: unknown[]) => unknown>;
for (const name of ["readFileSync", "existsSync", "statSync", "realpathSync"]) {
  const original = hooked[name];
  hooked[name] = (path: unknown, ...rest: unknown[]) => {
    if (typeof path === "string") read.add(path);
    return original(path, ...rest);
  };
}
const { analyzeAot } = await import(
  join(pocket, "microts/compiler/aot-build.ts")
);
const staged = join(pocket, "apps/.live-editor");
for (const app of apps) {
  fs.rmSync(staged, { recursive: true, force: true });
  fs.mkdirSync(staged, { recursive: true });
  for (const name of ["app.tsx", "app.d.ts", "pocket.json"])
    if (fs.existsSync(join(app, name)))
      fs.copyFileSync(join(app, name), join(staged, name));
  analyzeAot(join(staged, "app.tsx"), { strict: true });
}
fs.rmSync(staged, { recursive: true, force: true });
const snapshot: Record<string, string> = {};
for (const path of [...read].sort()) {
  if (path.startsWith(`${staged}/`)) continue;
  try {
    if (fs.statSync(path).isFile())
      snapshot[path] = fs.readFileSync(path, "utf8");
  } catch {
    // read as missing: the browser finds it missing too
  }
}
// The staged compiler reaches its dependencies through a link to the checkout's node_modules,
// so they were read there; the browser has no links and looks for them under the staged root.
const modules = fs.realpathSync(join(pocket, "node_modules"));
for (const [path, text] of Object.entries(snapshot))
  if (path.startsWith(`${modules}/`))
    snapshot[join(pocket, "node_modules", path.slice(modules.length))] = text;
writeFileSync(join(dist, "microts-files.json"), JSON.stringify(snapshot));
console.log(`microts-files.json: ${Object.keys(snapshot).length} files`);

// The browser entry: the compiler with `node:fs` reading the snapshot.
const entry = join(dist, ".microts-entry.ts");
writeFileSync(
  entry,
  `import ts from ${JSON.stringify(join(pocket, "node_modules/typescript/lib/typescript.js"))};
import { analyzeAot } from ${JSON.stringify(join(pocket, "microts/compiler/aot-build.ts"))};
import { clear, mount, put, system } from ${JSON.stringify(join(web, "src/files.ts"))};
import type { PocketCompiler } from ${JSON.stringify(join(web, "src/microts.ts"))};
/** Loads the snapshot build.ts recorded and answers the compiler over it. */
export function createCompiler(snapshot: Record<string, string>): PocketCompiler {
  mount(snapshot);
  (ts as unknown as { setSys(sys: unknown): void }).setSys(system(ts as never, ${JSON.stringify(pocket)}));
  return { analyzeAot, root: ${JSON.stringify(pocket)}, files: { put, clear } };
}
`,
);
const shim = join(web, "src/files.ts");
const result = await Bun.build({
  entrypoints: [entry],
  outdir: dist,
  naming: "microts.js",
  target: "browser",
  format: "esm",
  minify: true,
  plugins: [
    {
      name: "browser-node",
      setup(build) {
        build.onResolve({ filter: /^(node:)?(fs|fs\/promises)$/ }, () => ({
          path: shim,
        }));
        // The compiler imports these for its command line and its Rust output; compiling a view
        // never calls them.
        build.onResolve(
          {
            filter:
              /^(node:)?(zlib|crypto|util|os|inspector|perf_hooks|child_process|worker_threads|module)$/,
          },
          (args) => ({ path: args.path, namespace: "unused" }),
        );
        build.onLoad({ filter: /.*/, namespace: "unused" }, () => ({
          contents:
            "export default {}; export const inspect = (v) => String(v); export const createHash = () => { throw new Error('unavailable'); }; export const gzipSync = () => { throw new Error('unavailable'); };",
          loader: "js",
        }));
        // Bun's import.meta.dir is each module's own directory on disk.
        build.onLoad(
          { filter: /\/(microts|framework|contracts)\/.*\.ts$/ },
          async (args) => ({
            contents: (await Bun.file(args.path).text()).replaceAll(
              "import.meta.dir",
              JSON.stringify(dirname(args.path)),
            ),
            loader: "ts",
          }),
        );
      },
    },
  ],
});
fs.rmSync(entry, { force: true });
if (!result.success)
  throw new AggregateError(result.logs, "the MicroTS bundle failed");
console.log(`microts.js: ${(result.outputs[0].size / 1e6).toFixed(1)} MB`);

/** The page, its workers, and the files they load. */
async function buildApp() {
  const src = join(web, "src");
  const app = await Bun.build({
    entrypoints: [
      join(src, "main.ts"),
      join(src, "workers/build.ts"),
      join(src, "workers/run.ts"),
      join(src, "app.css"),
    ],
    root: src,
    outdir: dist,
    target: "browser",
    format: "esm",
    splitting: false,
    minify: true,
    sourcemap: "linked",
    plugins: [
      {
        // wasmoon's Emscripten glue imports `module` only when it runs on Node.
        name: "node-only",
        setup(build) {
          build.onResolve({ filter: /^(node:)?module$/ }, () => ({
            path: "module",
            namespace: "node-only",
          }));
          build.onLoad({ filter: /.*/, namespace: "node-only" }, () => ({
            contents:
              "export const createRequire = () => { throw new Error('unavailable in the browser'); };",
            loader: "js",
          }));
        },
      },
    ],
  });
  if (!app.success) throw new AggregateError(app.logs, "the app bundle failed");
  for (const name of ["index.html", "favicon.svg"])
    fs.copyFileSync(join(src, name), join(dist, name));
  fs.cpSync(join(src, "assets"), join(dist, "assets"), { recursive: true });
  fs.copyFileSync(
    join(repository, "node_modules/wasmoon/dist/glue.wasm"),
    join(dist, "lua.wasm"),
  );
  const fonts = join(
    repository,
    "plugins/system-fonts/filesystem/resources/fonts",
  );
  const names = readdirSync(fonts)
    .filter((name) => /^font-\d+\.bin$/.test(name))
    .sort();
  mkdirSync(join(dist, "fonts"), { recursive: true });
  for (const name of names)
    fs.copyFileSync(join(fonts, name), join(dist, "fonts", name));
  writeFileSync(join(dist, "fonts/index.json"), JSON.stringify(names));
  const examples = join(repository, "plugins/app-manager/resources/examples");
  const templates: Record<string, Record<string, string>> = {};
  for (const id of readdirSync(examples).sort()) {
    templates[id] = {};
    for (const file of readdirSync(join(examples, id)).sort())
      templates[id][file] = fs.readFileSync(join(examples, id, file), "utf8");
  }
  writeFileSync(join(dist, "templates.json"), JSON.stringify(templates));
  console.log(
    `app: main.js, workers, ${names.length} fonts, ${Object.keys(templates).length} templates`,
  );
}
