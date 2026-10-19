/**
 * The build worker: builds an app container from its sources off the page's thread. It loads the
 * MicroTS compiler and the WebAssembly linker once, then answers each `BuildRequest` with the
 * container's files or the first problem, with where it is.
 */

import { LuaFactory } from "wasmoon";
import { BuildError, buildContainer } from "../container";
import { compileView, type PocketCompiler } from "../microts";
import type { Files } from "../state/project";
import { loadModule, type Module } from "../wasm";

export interface BuildRequest {
  id: number;
  files: Files;
}

/** Where a build problem is: a file, and a line and column when the compiler names them. */
export interface Problem {
  message: string;
  file?: string;
  line?: number;
  column?: number;
}

export type BuildReply =
  | {
      id: number;
      ok: true;
      files: Record<string, Uint8Array>;
      manifest: Record<string, unknown>;
      warnings: Problem[];
      ms: number;
    }
  | { id: number; ok: false; problem: Problem; ms: number };

const base = new URL("../", import.meta.url);
let tools:
  | Promise<{ compiler: PocketCompiler; module: Module; lua: LuaFactory }>
  | undefined;

async function load() {
  const [{ createCompiler }, snapshot, module] = await Promise.all([
    import(/* @vite-ignore */ new URL("microts.js", base).href) as Promise<{
      createCompiler(snapshot: Record<string, string>): PocketCompiler;
    }>,
    fetch(new URL("microts-files.json", base)).then(
      (reply) => reply.json() as Promise<Record<string, string>>,
    ),
    loadModule(new URL("live-editor.wasm", base)),
  ]);
  return {
    compiler: createCompiler(snapshot),
    module,
    lua: new LuaFactory(new URL("lua.wasm", base).href),
  };
}

/** Checks the entry program compiles, as the device's VM would load it. */
async function checkLua(
  lua: LuaFactory,
  name: string,
  source: string,
): Promise<Problem | null> {
  const engine = await lua.createEngine({
    injectObjects: false,
    enableProxy: false,
  });
  engine.global.set("__source", source);
  engine.global.set("__name", `@${name}`);
  const message = (await engine.doString(
    "local _, problem = load(__source, __name, 't') return problem",
  )) as string | null;
  engine.global.close();
  if (!message) return null;
  const match = /^([^:]+):(\d+): (.*)$/s.exec(message);
  return match
    ? { message, file: match[1], line: Number(match[2]) }
    : { message, file: name };
}

async function build(request: BuildRequest): Promise<BuildReply> {
  const started = performance.now();
  const elapsed = () => Math.round(performance.now() - started);
  try {
    tools ??= load();
    const { compiler, module, lua } = await tools;
    const sources = new Map(Object.entries(request.files));
    let warnings: Problem[] = [];
    const container = await buildContainer(sources, {
      compileView: (view) => {
        const compiled = compileView(compiler, view);
        warnings = compiled.warnings.map((warning) => ({
          message: warning.message,
          file: warning.file,
          line: warning.line,
          column: warning.column,
        }));
        return compiled;
      },
      linkView: (view) => module.link(view),
    });
    const entry =
      typeof container.manifest.entry === "string"
        ? container.manifest.entry
        : "main.lua";
    const syntax = await checkLua(lua, entry, request.files[entry] ?? "");
    if (syntax)
      return { id: request.id, ok: false, problem: syntax, ms: elapsed() };
    return {
      id: request.id,
      ok: true,
      files: Object.fromEntries(container.files),
      manifest: container.manifest,
      warnings,
      ms: elapsed(),
    };
  } catch (error) {
    if (error instanceof BuildError)
      return {
        id: request.id,
        ok: false,
        problem: {
          message: error.message,
          file: error.file,
          line: error.line,
          column: error.column,
        },
        ms: elapsed(),
      };
    return {
      id: request.id,
      ok: false,
      problem: {
        message: String(error instanceof Error ? error.message : error),
      },
      ms: elapsed(),
    };
  }
}

self.onmessage = async (event: MessageEvent<BuildRequest>) => {
  (self as unknown as Worker).postMessage(await build(event.data));
};
