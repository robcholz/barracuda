import { expect, test } from "bun:test";
import { readdir } from "node:fs/promises";
import { resolve } from "node:path";
import { BOARDS, CHIPS } from "../../website/src/content/boards";

const root = resolve(import.meta.dir, "../..");

test("the website lists every Board in the repository, each with a docs page", async () => {
  const configs = (
    await readdir(resolve(root, "boards/configs"), { withFileTypes: true })
  )
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
  expect(BOARDS.map((board) => board.id).sort()).toEqual(configs);

  const pages = new Set(
    (await readdir(resolve(root, "docs/boards/supported"))).map((file) =>
      file.replace(/\.mdx$/, ""),
    ),
  );
  expect(BOARDS.filter((board) => !pages.has(board.id))).toEqual([]);
});

test("every Board's chip has a filter on the boards page", () => {
  expect(BOARDS.filter((board) => !CHIPS.includes(board.chip))).toEqual([]);
});
