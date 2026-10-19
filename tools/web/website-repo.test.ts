import { expect, test } from "bun:test";
import { REPO_TS, renderRepo, repoCounts } from "./website-repo";

test("the website's counts match the repository", async () => {
  // Out of date: run `bun tools/web/website-repo.ts`.
  expect(renderRepo(await repoCounts())).toBe(await Bun.file(REPO_TS).text());
});
