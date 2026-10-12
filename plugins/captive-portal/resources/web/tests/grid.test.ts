import { expect, test } from "bun:test";
import { fitColumns } from "../ui";

test("fitColumns keeps count columns while each fits min, then reflows", () => {
  expect(fitColumns(3, 160, "var(--space-3)")).toBe(
    "repeat(auto-fill,minmax(max(min(100%,160px),calc((100% - 2 * var(--space-3)) / 3)),1fr))",
  );
  expect(fitColumns(1, 180, "8px")).toBe(
    "repeat(auto-fill,minmax(max(min(100%,180px),100%),1fr))",
  );
});
