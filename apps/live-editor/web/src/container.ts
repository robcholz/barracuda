/**
 * Builds an app container from its sources in the browser, as `build-app.ts` builds one with the
 * toolchain: the manifest with `view` filled in, the entry program, and for an app with a view
 * `view.bvir` and `styles.bin`. Font atlases are not baked yet, so a view draws with the device's
 * system fonts.
 */

import { manifestProblem } from "./manifest";

/** An app's sources by file name: `app.json`, its entry program, and for a view `app.tsx`, `app.d.ts` and `pocket.json`. */
export type AppSources = ReadonlyMap<string, string>;

/** What the MicroTS compiler makes of a view: its View IR as JSON, and its compiled stylesheet. */
export interface CompiledView {
  view: string;
  styles: Uint8Array;
}

/** The two steps a view goes through; the browser runs both. */
export interface Toolchain {
  /** Compiles `app.tsx` against its `app.d.ts` contract. */
  compileView(sources: AppSources): Promise<CompiledView> | CompiledView;
  /** Links View IR JSON into BVIR, as `ui-link` does. */
  linkView(view: string): Uint8Array;
}

/** Why an app's sources do not build; `file` names the source at fault, when one is. */
export class BuildError extends Error {
  constructor(
    message: string,
    readonly file?: string,
    readonly line?: number,
    readonly column?: number,
  ) {
    super(message);
  }
}

/** The manifest's, the entry program's and the view files' size limits on the device. */
const MAX_MANIFEST_BYTES = 4 * 1024;
const MAX_ENTRY_BYTES = 64 * 1024;
const MAX_VIEW_BYTES = 512 * 1024;

interface Manifest {
  entry?: string;
  view?: {
    program?: string;
    styles?: string;
    fonts?: string[];
    size?: unknown;
  };
  [key: string]: unknown;
}

const utf8 = (text: string) => new TextEncoder().encode(text);

function json<T>(sources: AppSources, file: string): T {
  const text = sources.get(file);
  if (text === undefined) throw new BuildError(`the app has no ${file}`, file);
  try {
    return JSON.parse(text) as T;
  } catch (error) {
    throw new BuildError(`${file} is not valid JSON: ${error}`, file);
  }
}

/** Builds the container: its manifest and every file it names. */
export async function buildContainer(
  sources: AppSources,
  toolchain: Toolchain,
): Promise<{ manifest: Manifest; files: Map<string, Uint8Array> }> {
  // The build fills in the manifest's `view` once the view exists.
  const manifest = json<Manifest>(sources, "app.json");
  delete manifest.view;
  const entry = manifest.entry ?? "main.lua";
  const program = sources.get(entry);
  if (program === undefined)
    throw new BuildError(`the app has no ${entry}`, entry);
  const files = new Map<string, Uint8Array>([[entry, utf8(program)]]);
  if (sources.has("app.tsx")) {
    const compiled = await toolchain.compileView(sources);
    let bvir: Uint8Array;
    try {
      bvir = toolchain.linkView(compiled.view);
    } catch (error) {
      throw new BuildError(
        `app.tsx: ${error instanceof Error ? error.message : error}`,
        "app.tsx",
      );
    }
    files.set("view.bvir", bvir);
    files.set("styles.bin", compiled.styles);
    manifest.view = { program: "view.bvir", styles: "styles.bin" };
    const pocket = sources.has("pocket.json")
      ? json<{ app?: { viewport?: { fixed?: { logical?: unknown } } } }>(
          sources,
          "pocket.json",
        )
      : {};
    const size = pocket.app?.viewport?.fixed?.logical;
    if (size) manifest.view.size = size;
  }
  const problem = manifestProblem(manifest);
  if (problem) throw new BuildError(`app.json: ${problem}`, "app.json");
  const text = utf8(`${JSON.stringify(manifest, null, 2)}\n`);
  if (text.length > MAX_MANIFEST_BYTES)
    throw new BuildError("app.json: the manifest is at most 4 KiB", "app.json");
  if ((files.get(entry)?.length ?? 0) > MAX_ENTRY_BYTES)
    throw new BuildError(
      `${entry}: the entry program is at most 64 KiB`,
      entry,
    );
  const view =
    (files.get("view.bvir")?.length ?? 0) +
    (files.get("styles.bin")?.length ?? 0);
  if (view > MAX_VIEW_BYTES)
    throw new BuildError(
      "app.tsx: the view files are at most 512 KiB together",
      "app.tsx",
    );
  files.set("app.json", text);
  return { manifest, files };
}
