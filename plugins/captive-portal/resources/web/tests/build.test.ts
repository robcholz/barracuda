import { expect, test } from "bun:test";

test("device output is self-contained, carries the kernel once and imports plugins dynamically", async () => {
  const resource = (name: string) =>
    Bun.file(new URL(`../../../filesystem/resources/${name}`, import.meta.url));
  const html = await resource("index.html").text();
  const js = await resource("app.js").text();
  const css = await resource("app.css").text();
  expect(html).toContain("/portal/app.js");
  expect(html).toContain("/portal/app.css");
  expect(js).toContain("import(");
  expect(js).toContain("data-hairline-style");
  expect(js).toContain("hl-figure");
  // no fetches leave the device (the SVG namespace in the favicon is a name, not a request)
  expect(
    (html + css + js).replaceAll("http://www.w3.org/2000/svg", ""),
  ).not.toMatch(/https?:\/\/(?!\/)/);
  expect(css).toContain("--sidebar-width");
  expect(css).toContain(".bc-toaster");
  // the design system's answers to its gaps ship with it
  for (const name of [
    ".bc-icon-button",
    ".bc-code-display",
    ".bc-mobile-title",
  ])
    expect(css).toContain(name);
  // phone inputs are 16px, so the browser never zooms into them
  expect(css).toMatch(
    /@media \(max-width: ?719px\) ?\{ ?\.bc-input ?\{ ?font-size: ?16px/,
  );
  expect(
    resource("index.html").size +
      resource("app.js").size +
      resource("app.css").size,
  ).toBeLessThan(96 * 1024);
});
