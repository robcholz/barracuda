import { expect, test } from "bun:test";
import { MAX_IN_FLIGHT, queuedFetch, queuedImport } from "../src/net";

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test("no more than MAX_IN_FLIGHT requests reach the device at once", async () => {
  let open = 0;
  let peak = 0;
  const release: (() => void)[] = [];
  const fetch = queuedFetch(async () => {
    open++;
    peak = Math.max(peak, open);
    await new Promise<void>((resolve) => release.push(resolve));
    open--;
    return new Response("ok");
  });
  const all = Promise.all(
    Array.from({ length: 9 }, (_, index) => fetch(`/icon-${index}.svg`)),
  );
  for (let round = 0; round < 20 && release.length < 9; round++) {
    await tick();
    release.splice(0).forEach((resolve) => resolve());
  }
  await all;
  expect(peak).toBe(MAX_IN_FLIGHT);
});

test("a refused GET is retried; a refused POST is not", async () => {
  let attempts = 0;
  const fetch = queuedFetch(async () => {
    if (++attempts < 3) throw new TypeError("Failed to fetch");
    return new Response("ok");
  });
  expect(await (await fetch("/portal/status")).text()).toBe("ok");
  expect(attempts).toBe(3);

  attempts = 0;
  await expect(fetch("/api/wifi", { method: "POST" })).rejects.toThrow(
    TypeError,
  );
  await tick();
  expect(attempts).toBe(1);
});

test("a failed module load is retried with a fresh URL", async () => {
  const urls: string[] = [];
  const load = queuedImport(async (url) => {
    urls.push(url);
    if (urls.length < 2) throw new TypeError("Failed to fetch module");
    return { mount() {} };
  });
  await load("/portal/assets/wifi/entry.js");
  expect(urls).toEqual([
    "/portal/assets/wifi/entry.js",
    "/portal/assets/wifi/entry.js?r=1",
  ]);
});
