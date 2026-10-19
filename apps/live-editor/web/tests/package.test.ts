import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  newSigningKey,
  pack,
  PackageError,
  packageFiles,
  signerName,
} from "../src/package";

const repository = resolve(import.meta.dir, "../../../..");
const PACK_APP = join(repository, "plugins/app-manager/resources/pack-app.ts");
const FIXTURES = join(
  repository,
  "plugins/app-manager/crates/runtime/tests/fixtures",
);

/** The counter's container: its manifest, its program, and the linked view the fixtures hold. */
function counter(): Map<string, Uint8Array> {
  const manifest = {
    format: 1,
    id: "counter",
    name: "Counter",
    version: "0.1.0",
    view: { program: "view.bvir", styles: "styles.bin" },
  };
  return new Map([
    [
      "app.json",
      new TextEncoder().encode(`${JSON.stringify(manifest, null, 2)}\n`),
    ],
    ["main.lua", new TextEncoder().encode('print("counter")\n')],
    ["view.bvir", readFileSync(join(FIXTURES, "counter.bvir"))],
    ["styles.bin", readFileSync(join(FIXTURES, "counter.styles.bin"))],
  ]);
}

/** Packs `files` with the toolchain's pack-app.ts, as a developer would. */
function packAppTs(files: Map<string, Uint8Array>, key?: string) {
  const bundle = mkdtempSync(join(tmpdir(), "live-editor-pack-"));
  for (const [name, bytes] of files) writeFileSync(join(bundle, name), bytes);
  const out = join(bundle, "out.bapp");
  const run = Bun.spawnSync(
    ["bun", PACK_APP, bundle, out, ...(key ? ["--key", key] : [])],
    { stderr: "inherit" },
  );
  expect(run.exitCode).toBe(0);
  return {
    bytes: new Uint8Array(readFileSync(out)),
    said: run.stdout.toString(),
  };
}

test("an unsigned package is the one pack-app.ts makes", async () => {
  const files = counter();
  const packed = await pack(packageFiles(files));
  expect(packed).toEqual(packAppTs(files).bytes);
});

test("a signed package carries the key and a signature over everything before", async () => {
  const files = counter();
  const key = await newSigningKey();
  const packed = await pack(packageFiles(files), key);
  const unsigned = await pack(packageFiles(files));
  // Only the signed flag differs before the signature block.
  expect(packed[6]).toBe(1);
  const content = packed.slice(0, unsigned.length);
  expect(content.slice(7)).toEqual(unsigned.slice(7));
  expect(
    new TextDecoder().decode(
      packed.slice(unsigned.length, unsigned.length + 4),
    ),
  ).toBe("BSIG");
  const raw = packed.slice(unsigned.length + 4, unsigned.length + 69);
  const signature = packed.slice(unsigned.length + 69);
  const verifier = await crypto.subtle.importKey(
    "raw",
    raw,
    { name: "ECDSA", namedCurve: "P-256" },
    false,
    ["verify"],
  );
  expect(
    await crypto.subtle.verify(
      { name: "ECDSA", hash: "SHA-256" },
      verifier,
      signature,
      content,
    ),
  ).toBe(true);

  // The signer has the name pack-app.ts prints for the same key, as a device shows it.
  const jwk = join(mkdtempSync(join(tmpdir(), "live-editor-key-")), "key.jwk");
  writeFileSync(
    jwk,
    JSON.stringify(await crypto.subtle.exportKey("jwk", key.privateKey)),
  );
  const name = await signerName(key.publicKey);
  expect(name).toMatch(/^[0-9a-f]{16}$/);
  expect(packAppTs(files, jwk).said).toContain(`signed by ${name}`);
});

test("a container that cannot be packed says why", async () => {
  const files = counter();
  files.delete("styles.bin");
  expect(() => packageFiles(files)).toThrow(
    new PackageError("the container has no styles.bin"),
  );
  await expect(
    pack([
      {
        name: "a-file-name-longer-than-thirty-two.bin",
        bytes: new Uint8Array(),
      },
    ]),
  ).rejects.toThrow("at most 32 bytes");
  await expect(
    pack([{ name: "big.bin", bytes: new Uint8Array(1024 * 1024 + 1) }]),
  ).rejects.toThrow("a device takes at most");
});
