import { describe, expect, it } from "bun:test";
import { heal as remend } from "../../markdown/heal";

describe("basic input handling", () => {
  it("should return empty string unchanged", () => {
    expect(remend("")).toBe("");
  });

  it("should return regular text unchanged", () => {
    const text = "This is plain text without any markdown";
    expect(remend(text)).toBe(text);
  });
});
