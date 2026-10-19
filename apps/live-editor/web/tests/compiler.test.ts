// The compiler build.ts bundles: the WebAssembly linker and PocketJS's MicroTS compiler. These
// run once `bun apps/live-editor/web/build.ts` has built dist/, which needs a PocketJS checkout.
import { expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { buildContainer } from "../src/container";
import { linker, LinkError, loadLinker } from "../src/linker";
import { compileView, type PocketCompiler } from "../src/microts";
import { pack, packageFiles } from "../src/package";

const repository = resolve(import.meta.dir, "../../../..");
const dist = resolve(import.meta.dir, "../dist");
const FIXTURES = join(
  repository,
  "plugins/app-manager/crates/runtime/tests/fixtures",
);
const built = existsSync(join(dist, "microts.js"));

/** A module that speaks the linker's ABI over plain memory, answering `reply` for any input. */
function fakeModule(reply: { status: number; bytes: Uint8Array }) {
  const memory = new WebAssembly.Memory({ initial: 1 });
  let seen = "";
  return {
    seen: () => seen,
    exports: {
      memory,
      // Grows memory as the real module may, so a view taken before the call would be detached.
      input: (len: number) => {
        seen = String(len);
        memory.grow(1);
        return 16;
      },
      link: () => {
        seen = new TextDecoder().decode(
          new Uint8Array(memory.buffer, 16, Number(seen)),
        );
        new Uint8Array(memory.buffer, 1024, reply.bytes.length).set(
          reply.bytes,
        );
        return reply.status;
      },
      output: () => 1024,
      output_len: () => reply.bytes.length,
    } as unknown as WebAssembly.Exports,
  };
}

test("the linker passes View IR in and BVIR or the reason out", () => {
  const ok = fakeModule({ status: 0, bytes: new Uint8Array([9, 8, 7]) });
  expect(linker(ok.exports)('{"version":1}')).toEqual(
    new Uint8Array([9, 8, 7]),
  );
  expect(ok.seen()).toBe('{"version":1}');
  const refused = fakeModule({
    status: 1,
    bytes: new TextEncoder().encode("the view has too many nodes"),
  });
  expect(() => linker(refused.exports)("{}")).toThrow(
    new LinkError("the view has too many nodes"),
  );
});

test.if(built)("the WebAssembly linker links as ui-link does", async () => {
  const link = await loadLinker(readFileSync(join(dist, "compiler.wasm")));
  expect(
    link(readFileSync(join(FIXTURES, "counter.view.json"), "utf8")),
  ).toEqual(new Uint8Array(readFileSync(join(FIXTURES, "counter.bvir"))));
  expect(() => link("not json")).toThrow("the view is not View IR JSON");
});

async function browserCompiler(): Promise<PocketCompiler> {
  const { createCompiler } = await import(join(dist, "microts.js"));
  return createCompiler(
    JSON.parse(readFileSync(join(dist, "microts-files.json"), "utf8")),
  );
}

const sources = (directory: string) =>
  new Map(
    readdirSync(directory).map((name) => [
      name,
      readFileSync(join(directory, name), "utf8"),
    ]),
  );

test.if(built)(
  "a view compiled in the browser is the one the toolchain committed",
  async () => {
    const compiler = await browserCompiler();
    const link = await loadLinker(readFileSync(join(dist, "compiler.wasm")));
    const view = join(repository, "plugins/app-manager/resources/view");
    const committed = join(
      repository,
      "plugins/app-manager/filesystem/resources/view",
    );
    const compiled = compileView(compiler, sources(view));
    expect(compiled.styles).toEqual(
      new Uint8Array(readFileSync(join(committed, "styles.bin"))),
    );
    expect(link(compiled.view)).toEqual(
      new Uint8Array(readFileSync(join(committed, "view.bvir"))),
    );
  },
);

test.if(built)(
  "an example app builds and packs in the browser, and a broken one is explained",
  async () => {
    const compiler = await browserCompiler();
    const link = await loadLinker(readFileSync(join(dist, "compiler.wasm")));
    const toolchain = {
      compileView: (app: Map<string, string>) => compileView(compiler, app),
      linkView: link,
    };
    const counter = sources(
      join(repository, "plugins/app-manager/resources/examples/counter"),
    );
    const { files } = await buildContainer(counter, toolchain);
    const packed = await pack(packageFiles(files));
    expect(new TextDecoder().decode(packed.slice(0, 4))).toBe("BAPP");

    counter.set(
      "app.tsx",
      `${counter.get("app.tsx")}\nconst broken: number = "text";\n`,
    );
    await expect(buildContainer(counter, toolchain)).rejects.toMatchObject({
      file: "app.tsx",
    });
  },
);
