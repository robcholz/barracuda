/**
 * The MicroTS compiler in the browser. PocketJS's compiler reads its own sources and TypeScript's
 * library declarations from disk; build.ts bundles it with those files in memory (`files.ts`), and
 * this module runs it on an app's sources as `build-app.ts` runs it on disk.
 */

import { BuildError, type AppSources, type CompiledView } from "./container";

/** A MicroTS diagnostic: where it is and what is wrong. */
interface Diagnostic {
  file: string;
  line: number;
  column: number;
  severity: "error" | "warning";
  message: string;
}

/** The View IR program `analyzeAot` answers; only these parts reach a device. */
interface AotProgram {
  version: number;
  types: unknown[];
  components: unknown[];
  styles: { bytes: ArrayLike<number> };
  diagnostics: Diagnostic[];
}

/** What build.ts bundles from a PocketJS checkout. */
export interface PocketCompiler {
  /** PocketJS's `analyzeAot`, reading through `files.ts`. */
  analyzeAot(entry: string, options: { strict: boolean }): AotProgram;
  /** Where the checkout sits in the in-memory file system. */
  root: string;
  /** The in-memory file system the bundled compiler reads (its own copy of `files.ts`). */
  files: {
    put(path: string, text: string): void;
    clear(directory: string): void;
  };
}

/** Source locations are kept out of the IR a device reads, as `build-app.ts` keeps them out. */
function strip(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(strip);
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value)
        .filter(([key]) => key !== "loc" && key !== "file")
        .map(([key, item]) => [key, strip(item)]),
    );
  return value;
}

const VIEW_SOURCES = ["app.tsx", "app.d.ts", "pocket.json"];

/**
 * Compiles an app's view. The sources go where `build-app.ts` stages them, inside the PocketJS
 * checkout, so the view's `@pocketjs/*` imports resolve.
 */
export function compileView(
  compiler: PocketCompiler,
  sources: AppSources,
): CompiledView & { warnings: Diagnostic[] } {
  const staged = `${compiler.root}/apps/.live-editor`;
  compiler.files.clear(staged);
  for (const name of VIEW_SOURCES) {
    const text = sources.get(name);
    if (text !== undefined) compiler.files.put(`${staged}/${name}`, text);
  }
  const local = (diagnostic: Diagnostic) => ({
    ...diagnostic,
    file: diagnostic.file.startsWith(`${staged}/`)
      ? diagnostic.file.slice(staged.length + 1)
      : diagnostic.file,
  });
  let program: AotProgram;
  try {
    program = compiler.analyzeAot(`${staged}/app.tsx`, { strict: true });
  } catch (error) {
    const diagnostics = (error as { diagnostics?: Diagnostic[] }).diagnostics;
    const first = diagnostics?.map(local).find((d) => d.severity === "error");
    if (first)
      throw new BuildError(
        `${first.file}:${first.line}:${first.column}: ${first.message}`,
        first.file,
        first.line,
        first.column,
      );
    throw new BuildError(
      error instanceof Error ? error.message : String(error),
      "app.tsx",
    );
  }
  return {
    view: JSON.stringify(
      strip({
        version: program.version,
        types: program.types,
        components: program.components,
      }),
    ),
    styles: Uint8Array.from(program.styles.bytes),
    warnings: program.diagnostics.map(local),
  };
}
