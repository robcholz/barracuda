import { expect, test } from "bun:test";
import { parseEntries, ModuleSession } from "../src/runtime";

const entry = {
  id: "wifi",
  title: "Wi-Fi",
  module: "/portal/assets/wifi/entry.js",
};
test("manifest accepts registered paths and rejects foreign code and duplicate IDs", () => {
  expect(parseEntries([entry])).toEqual([entry]);
  for (const module of [
    "https://evil.test/a.js",
    "/portal/assets/other/a.js",
    "/portal/assets/wifi/../a.js",
    "/portal/assets/wifi/%2e/a.js",
  ]) {
    expect(() => parseEntries([{ ...entry, module }])).toThrow();
  }
  expect(() => parseEntries([entry, entry])).toThrow();
});

test("switching aborts the old module and calls cleanup", async () => {
  const session = new ModuleSession();
  let disposed = 0;
  let signal: AbortSignal | undefined;
  await session.open(entry, {} as HTMLElement, async () => ({
    mount: (_root: HTMLElement, context: { signal: AbortSignal }) => {
      signal = context.signal;
      return () => {
        disposed++;
      };
    },
  }));
  session.close();
  expect(signal?.aborted).toBe(true);
  expect(disposed).toBe(1);
  session.close();
  expect(disposed).toBe(1);
});

test("an import finishing after navigation does not mount", async () => {
  const session = new ModuleSession();
  let release!: (value: unknown) => void;
  let mounted = false;
  const pending = session.open(
    entry,
    {} as HTMLElement,
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  session.close();
  release({
    mount: () => {
      mounted = true;
    },
  });
  await pending;
  expect(mounted).toBe(false);
});

test("late async mount cleanup is not lost", async () => {
  const session = new ModuleSession();
  let release!: (cleanup: () => void) => void;
  let cleaned = false;
  const pending = session.open(entry, {} as HTMLElement, async () => ({
    mount: () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  }));
  await Promise.resolve();
  session.close();
  release(() => {
    cleaned = true;
  });
  await pending;
  expect(cleaned).toBe(true);
});
