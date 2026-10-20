// The editor's two languages: which one it starts in, and that both say everything.
import { expect, test } from "bun:test";
import { initialLang, STRINGS } from "../src/i18n";

test("the website's ?lang= wins, then the one picked here, then the browser's", () => {
  expect(initialLang("?lang=zh", "en", ["en-US"])).toBe("zh");
  expect(initialLang("?lang=fr", "zh", ["en-US"])).toBe("zh");
  expect(initialLang("", null, ["fr-FR", "zh-CN", "en"])).toBe("zh");
  expect(initialLang("", null, ["zh-Hant-TW"])).toBe("zh");
  expect(initialLang("", null, ["fr-FR"])).toBe("en");
  expect(initialLang("", null, [])).toBe("en");
});

/** Each key path and the kind of value at it. */
function shape(value: unknown, path = ""): string[] {
  if (typeof value === "object" && value !== null && !Array.isArray(value))
    return Object.entries(value).flatMap(([key, inner]) =>
      shape(inner, `${path}.${key}`),
    );
  return [`${path}: ${Array.isArray(value) ? "array" : typeof value}`];
}

test("Chinese has every word English has, of the same kind", () => {
  expect(shape(STRINGS.zh)).toEqual(shape(STRINGS.en));
  expect(STRINGS.zh.ideas).toHaveLength(STRINGS.en.ideas.length);
  expect(STRINGS.zh.steps("x")).toHaveLength(STRINGS.en.steps("x").length);
});

test("the titles and notes the editor stores show in the reader's language", () => {
  expect(STRINGS.zh.stored("New chat")).toBe("新对话");
  expect(STRINGS.zh.stored("You edited app.tsx")).toBe("你编辑了 app.tsx");
  expect(STRINGS.zh.stored("Before deleting util.lua")).toBe(
    "删除 util.lua 之前",
  );
  expect(STRINGS.zh.stored("A kitchen timer")).toBe("A kitchen timer");
  expect(STRINGS.en.stored("New chat")).toBe("New chat");
});
