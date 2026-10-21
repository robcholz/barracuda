import { expect, test } from "bun:test";
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { BuildError, buildContainer, type Toolchain } from "../src/container";

const repository = resolve(import.meta.dir, "../../../..");
const COUNTER = join(
  repository,
  "plugins/app-manager/resources/examples/counter",
);

function sources(directory: string): Map<string, string> {
  return new Map(
    readdirSync(directory).map((name) => [
      name,
      readFileSync(join(directory, name), "utf8"),
    ]),
  );
}

/** A toolchain that records what it was given and answers fixed bytes. */
function fixedToolchain(): Toolchain & { linked: string[] } {
  const linked: string[] = [];
  return {
    linked,
    compileView: () => ({
      view: '{"version":1}',
      styles: new Uint8Array([7, 7]),
    }),
    linkView: (view) => {
      linked.push(view);
      return new Uint8Array([1, 2, 3]);
    },
  };
}

test("an app with a view builds into the container build-app.ts makes", async () => {
  const toolchain = fixedToolchain();
  const { manifest, files } = await buildContainer(sources(COUNTER), toolchain);
  const pocket = JSON.parse(readFileSync(join(COUNTER, "pocket.json"), "utf8"));
  expect(manifest.view).toEqual({
    program: "view.bvir",
    styles: "styles.bin",
    size: pocket.app.viewport.fixed.logical,
  });
  expect(toolchain.linked).toEqual(['{"version":1}']);
  expect([...files.keys()].sort()).toEqual([
    "app.json",
    "main.lua",
    "styles.bin",
    "view.bvir",
  ]);
  expect(files.get("view.bvir")).toEqual(new Uint8Array([1, 2, 3]));
  expect(new TextDecoder().decode(files.get("main.lua"))).toBe(
    readFileSync(join(COUNTER, "main.lua"), "utf8"),
  );
  const written = JSON.parse(new TextDecoder().decode(files.get("app.json")));
  expect(written).toEqual(manifest);
});

test("an app without a view builds headless", async () => {
  const app = new Map([
    ["app.json", '{"format":1,"id":"clock","name":"Clock","version":"1"}'],
    ["main.lua", "print(1)"],
  ]);
  const { manifest, files } = await buildContainer(app, fixedToolchain());
  expect(manifest.view).toBeUndefined();
  expect([...files.keys()].sort()).toEqual(["app.json", "main.lua"]);
});

test("what does not build names the source at fault", async () => {
  const app = sources(COUNTER);
  await expect(
    buildContainer(app, {
      ...fixedToolchain(),
      linkView: () => {
        throw new Error("views do not support refs yet");
      },
    }),
  ).rejects.toEqual(
    new BuildError("app.tsx: views do not support refs yet", "app.tsx"),
  );
  app.delete("main.lua");
  await expect(buildContainer(app, fixedToolchain())).rejects.toMatchObject({
    file: "main.lua",
  });
  app.set("app.json", "{");
  await expect(buildContainer(app, fixedToolchain())).rejects.toMatchObject({
    file: "app.json",
  });
});
