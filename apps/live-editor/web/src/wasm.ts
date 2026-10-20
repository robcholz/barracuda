/**
 * Live Editor's WebAssembly module, `barracuda-live-editor-wasm`: links View IR JSON into BVIR
 * exactly as `ui-link` does, and runs a linked view on the device's own UI core for the preview.
 * apps/live-editor/wasm/src/lib.rs documents the ABI.
 */

/** What the module exports. */
interface Exports {
  memory: WebAssembly.Memory;
  input(len: number): number;
  output(): number;
  output_len(): number;
  link(): number;
  stage(kind: number): void;
  open(width: number, height: number): number;
  contract(): void;
  set(): number;
  frame(): void;
  tick_rate(): number;
  draw(scale: number): number;
  press(x: number, y: number): number;
  texts(): void;
}

/** A view the linker refused; the message says why. */
export class LinkError extends Error {}

/** A view the preview could not open; the message says why. */
export class PreviewError extends Error {}

/** A handler call a tap selects, as the event Lua receives. */
export interface ViewCall {
  handler: string;
  args: unknown[];
}

/** A view's contract: its values and handlers, typed as `app.d.ts` types them. */
export interface ViewContract {
  values: Record<string, string>;
  handlers: Record<string, string[]>;
}

/** The view files the preview mounts. */
export interface ViewFiles {
  program: Uint8Array;
  styles: Uint8Array;
  fonts: readonly Uint8Array[];
}

/** One running view, drawn by the device's UI core. */
export interface Preview {
  readonly width: number;
  readonly height: number;
  contract(): ViewContract;
  /** Writes a value; answers `view:set`'s error, or `null` once written. */
  set(name: string, value: unknown): string | null;
  frame(): void;
  tickRate(): number;
  /** New RGBA pixels at `scale` pixels per view pixel, or `null` when the picture did not change. */
  draw(scale: number): Uint8Array | null;
  press(x: number, y: number): ViewCall | null;
  texts(): string[];
}

/** The module: the linker, and the preview over its one running view. */
export interface Module {
  link(view: string): Uint8Array;
  /** Mounts `files` at `width` × `height`, replacing the running view. */
  open(files: ViewFiles, width: number, height: number): Preview;
  /** Gives the module the device's system fonts, kept for every view it opens. */
  systemFonts(fonts: readonly Uint8Array[]): void;
}

/** Wraps an instantiated module's exports. */
export function module(exports: WebAssembly.Exports): Module {
  const wasm = exports as unknown as Exports;
  const write = (bytes: Uint8Array) => {
    // Size the buffer first: that may grow memory, and growing detaches the old view of it.
    const at = wasm.input(bytes.length);
    new Uint8Array(wasm.memory.buffer, at, bytes.length).set(bytes);
  };
  // Read after the call: it may have grown memory and moved its buffer.
  const output = () =>
    new Uint8Array(
      wasm.memory.buffer,
      wasm.output(),
      wasm.output_len(),
    ).slice();
  const text = () => new TextDecoder().decode(output());
  const utf8 = (value: string) => new TextEncoder().encode(value);
  let current = 0;
  return {
    link(view) {
      write(utf8(view));
      const status = wasm.link();
      const bytes = output();
      if (status !== 0) throw new LinkError(new TextDecoder().decode(bytes));
      return bytes;
    },
    systemFonts(fonts) {
      for (const font of fonts) {
        write(font);
        wasm.stage(3);
      }
    },
    open(files, width, height) {
      write(files.program);
      wasm.stage(0);
      write(files.styles);
      wasm.stage(1);
      for (const font of files.fonts) {
        write(font);
        wasm.stage(2);
      }
      if (wasm.open(width, height) !== 0) throw new PreviewError(text());
      const id = ++current;
      const live = () => {
        if (id !== current)
          throw new PreviewError("another view replaced this one");
      };
      return {
        width,
        height,
        contract() {
          live();
          wasm.contract();
          return JSON.parse(text()) as ViewContract;
        },
        set(name, value) {
          live();
          write(utf8(JSON.stringify({ name, value })));
          return wasm.set() === 0 ? null : text();
        },
        frame() {
          live();
          wasm.frame();
        },
        tickRate() {
          live();
          return wasm.tick_rate();
        },
        draw(scale) {
          live();
          return wasm.draw(scale) === 1 ? output() : null;
        },
        press(x, y) {
          live();
          return wasm.press(x, y) === 1
            ? (JSON.parse(text()) as ViewCall)
            : null;
        },
        texts() {
          live();
          wasm.texts();
          return JSON.parse(text()) as string[];
        },
      };
    },
  };
}

/** Wraps an instantiated module's exports as `link(json) → BVIR`. */
export function linker(
  exports: WebAssembly.Exports,
): (view: string) => Uint8Array {
  const wrapped = module(exports);
  return (view) => wrapped.link(view);
}

/** Loads the module from `source`, its URL or its bytes. */
export async function loadModule(
  source: URL | string | BufferSource,
): Promise<Module> {
  const bytes =
    source instanceof URL || typeof source === "string"
      ? await (await fetch(source)).arrayBuffer()
      : source;
  const instance = await WebAssembly.instantiate(
    await WebAssembly.compile(bytes),
    {},
  );
  return module(instance.exports);
}

/** Loads the module from `source` and answers its linker. */
export async function loadLinker(
  source: URL | string | BufferSource,
): Promise<(view: string) => Uint8Array> {
  const loaded = await loadModule(source);
  return (view) => loaded.link(view);
}
