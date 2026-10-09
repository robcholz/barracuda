import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import type { FigureDefinition } from "../src/contract";
import { installBrowser, settle } from "./browser";
import {
  defineFigureElement,
  loadFigure,
  setFigureSource,
} from "../src/live/host";

let browser: ReturnType<typeof installBrowser>;
beforeEach(() => {
  browser = installBrowser();
  defineFigureElement(browser.window);
});
afterEach(async () => {
  browser.document.body.replaceChildren();
  setFigureSource(() => null);
  await browser.close();
});

test("the kernel is published as the global HL", () => {
  const HL = (globalThis as unknown as { HL?: Record<string, unknown> }).HL;
  expect(typeof HL?.Cam).toBe("function");
  expect(typeof HL?.register).toBe("function");
});

test("a figure module is imported once per URL; a failed import can be retried", async () => {
  let imports = 0;
  let broken = true;
  const figure: FigureDefinition = {
    name: "loupe",
    range: [0, 1, 2],
    mount: () => ({ destroy() {} }),
  };
  const load = async () => {
    imports++;
    return broken ? { figure: { name: "x" } } : { figure };
  };
  await expect(loadFigure("/portal/assets/a/figure.js", load)).rejects.toThrow(
    /figure/,
  );
  await settle();
  broken = false;
  const [first, second] = await Promise.all([
    loadFigure("/portal/assets/a/figure.js", load),
    loadFigure("/portal/assets/a/figure.js", load),
  ]);
  expect(first).toBe(figure);
  expect(second).toBe(figure);
  expect(imports).toBe(2);
});

test("<hl-figure> mounts built-ins and entry figures, crops them, and destroys them on removal", async () => {
  const log: string[] = [];
  const figure = {
    name: "router",
    range: [0.5, 1.5, 3],
    mount: (_target: unknown, value: number) => {
      log.push(`mount ${value}`);
      return {
        destroy: () => log.push("destroy"),
        scan: (on: boolean) => log.push(`scan ${on}`),
      };
    },
  };
  setFigureSource(
    (name) =>
      name === "wifi" ? "/portal/assets/wifi/figure-host-test.js" : null,
    async () => ({ figure }),
  );
  const zone = browser.document.createElement("a");
  zone.setAttribute("data-hl-zone", "");
  const entryFigure = browser.document.createElement("hl-figure");
  entryFigure.setAttribute("name", "wifi");
  entryFigure.setAttribute("scan", "true");
  const board = browser.document.createElement("hl-figure");
  board.setAttribute("name", "board");
  board.setAttribute("aria-label", "Barracuda");
  const unknown = browser.document.createElement("hl-figure");
  unknown.setAttribute("name", "nothing");
  zone.append(entryFigure);
  browser.document.body.append(zone, board, unknown);
  await settle();
  expect(log).toEqual(["mount 1.5", "scan true"]);
  const stage = entryFigure.querySelector<HTMLElement>("[data-hairline]")!;
  expect(stage.getAttribute("data-hairline")).toBe("router");
  // the router's crop: 46.2 43.1 302 241.6 of its 400 × 320 frame
  expect(stage.style.width).toMatch(/^132\.45/);
  expect(entryFigure.getAttribute("aria-hidden")).toBe("true");
  expect(board.getAttribute("role")).toBe("img");
  expect(board.querySelectorAll("svg path").length).toBeGreaterThan(10);
  expect(unknown.childElementCount).toBe(0);
  entryFigure.setAttribute("scan", "false");
  expect(log.at(-1)).toBe("scan false");
  zone.remove();
  board.remove();
  expect(log.at(-1)).toBe("destroy");
  expect(board.childElementCount).toBe(0);
});

test("a figure whose mount throws leaves an empty plate", async () => {
  const errors = spyOn(console, "error").mockImplementation(() => {});
  try {
    setFigureSource(
      () => "/portal/assets/bad/figure.js",
      async () => ({
        figure: {
          name: "bad",
          range: [0, 1, 2],
          mount: () => {
            throw new Error("boom");
          },
        },
      }),
    );
    const node = browser.document.createElement("hl-figure");
    node.setAttribute("name", "bad");
    browser.document.body.append(node);
    await settle();
    expect(node.childElementCount).toBe(0);
    expect(errors).toHaveBeenCalled();
  } finally {
    errors.mockRestore();
  }
});
