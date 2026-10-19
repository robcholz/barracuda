/**
 * The view linker, `barracuda-live-editor-compiler` built to WebAssembly: links View IR JSON into
 * BVIR exactly as `ui-link` does.
 */

/** What the module exports; apps/live-editor/compiler/src/lib.rs documents the ABI. */
interface LinkerExports {
  memory: WebAssembly.Memory;
  input(len: number): number;
  link(): number;
  output(): number;
  output_len(): number;
}

/** A view the linker refused; the message says why. */
export class LinkError extends Error {}

/** Wraps an instantiated module's exports as `link(json) → BVIR`. */
export function linker(
  exports: WebAssembly.Exports,
): (view: string) => Uint8Array {
  const wasm = exports as unknown as LinkerExports;
  return (view) => {
    const input = new TextEncoder().encode(view);
    // Size the buffer first: that may grow memory, and growing detaches the old view of it.
    const at = wasm.input(input.length);
    new Uint8Array(wasm.memory.buffer, at, input.length).set(input);
    const status = wasm.link();
    // Read after linking: the call may have grown memory and moved its buffer.
    const output = new Uint8Array(
      wasm.memory.buffer,
      wasm.output(),
      wasm.output_len(),
    ).slice();
    if (status !== 0) throw new LinkError(new TextDecoder().decode(output));
    return output;
  };
}

/** Loads the module from `source` (its URL, or its bytes) and answers its linker. */
export async function loadLinker(
  source: URL | string | BufferSource,
): Promise<(view: string) => Uint8Array> {
  const bytes =
    source instanceof URL || typeof source === "string"
      ? await (await fetch(source)).arrayBuffer()
      : source;
  const instance = await WebAssembly.instantiate(
    await WebAssembly.compile(bytes),
    {},
  );
  return linker(instance.exports);
}
