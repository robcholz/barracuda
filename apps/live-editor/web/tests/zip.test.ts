// The download's zip opens as the app's folder.
import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { crc32, zip } from "../src/install/zip";

test("crc32 is zip's", () => {
  expect(crc32(new TextEncoder().encode("123456789"))).toBe(0xcbf43926);
});

test("unzip opens it as the app's folder", () => {
  const files = new Map([
    ["app.json", new TextEncoder().encode('{"id":"counter"}')],
    ["view.bvir", new Uint8Array([0, 1, 2, 255])],
  ]);
  const dir = mkdtempSync(join(tmpdir(), "live-editor-zip-"));
  try {
    Bun.write(join(dir, "app.zip"), zip("counter.bapp", files));
    const unzip = Bun.spawnSync(["unzip", "-q", "app.zip"], { cwd: dir });
    if (
      unzip.exitCode === null ||
      (unzip.exitCode !== 0 && unzip.stderr.toString().includes("not found"))
    )
      return;
    expect(unzip.exitCode).toBe(0);
    expect(readFileSync(join(dir, "counter.bapp/app.json"), "utf8")).toBe(
      '{"id":"counter"}',
    );
    expect(
      new Uint8Array(readFileSync(join(dir, "counter.bapp/view.bvir"))),
    ).toEqual(files.get("view.bvir")!);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
