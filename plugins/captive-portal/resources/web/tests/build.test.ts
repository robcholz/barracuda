import { expect, test } from "bun:test";

test("device output stays small, self-contained, and dynamically imports plugins", async () => {
  const resource = (name: string) =>
    Bun.file(new URL(`../../../filesystem/resources/${name}`, import.meta.url));
  const html = await resource("index.html").text();
  const js = await resource("app.js").text();
  const css = await resource("app.css").text();
  expect(html).toContain("/portal/app.js");
  expect(html).toContain("/portal/app.css");
  expect(js).toContain("import(");
  expect(html + css).not.toMatch(/https?:\/\//);
  expect(
    resource("index.html").size +
      resource("app.js").size +
      resource("app.css").size,
  ).toBeLessThan(32 * 1024);
});
