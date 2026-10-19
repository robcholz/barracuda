// The agent's tools and the history it sends.
import { expect, test } from "bun:test";
import { history } from "../src/agent/agent";
import type { Service } from "../src/agent/service";
import { checkInput, numbered, replaceOnce } from "../src/agent/tools";
import type { Chat } from "../src/state/project";

test("tool inputs are checked against their schemas", () => {
  expect(checkInput("read_file", { path: "app.tsx" })).toBeNull();
  expect(checkInput("read_file", {})).toBe("path is missing");
  expect(checkInput("tap", { x: 1.5, y: 2 })).toBe("x must be an integer");
  expect(checkInput("write_file", { path: "a", content: "b", extra: 1 })).toBe(
    "extra is not a parameter of write_file",
  );
  expect(checkInput("rm", {})).toBe("there is no tool named rm");
});

test("an edit replaces exactly one occurrence", () => {
  expect(replaceOnce("a b c", "b", "B")).toBe("a B c");
  expect(replaceOnce("a b b", "b", "B")).toEqual({
    error: "old_text occurs more than once; include more surrounding text",
  });
  expect(replaceOnce("a", "z", "B")).toEqual({
    error: "old_text does not occur in the file; read it again",
  });
  expect(numbered("x\ny")).toBe("1\tx\n2\ty");
});

const service = {
  id: "anthropic",
  user: (text: string) => ({ role: "user", content: text }),
} as unknown as Service;

test("history replays each finished turn as it went, and notes the person's edits", () => {
  const chat: Chat = {
    id: "c",
    title: "t",
    at: 0,
    items: [
      { kind: "user", id: "u1", text: "Add a button", attachments: [], at: 0 },
      {
        kind: "agent",
        id: "a1",
        at: 0,
        status: "done",
        steps: [{ text: "Done.", tools: [] }],
      },
      { kind: "note", id: "n1", text: "You edited app.tsx", at: 0 },
      { kind: "user", id: "u2", text: "Now red", attachments: [], at: 0 },
      {
        kind: "agent",
        id: "a2",
        at: 0,
        status: "stopped",
        steps: [{ text: "Partly", tools: [] }],
      },
      { kind: "user", id: "u3", text: "Again", attachments: [], at: 0 },
    ],
  };
  const kept = [
    {
      role: "assistant",
      content: [
        { type: "thinking", thinking: "", signature: "s" },
        { type: "text", text: "Done." },
      ],
    },
  ];
  expect(
    history(chat, { a1: { service: "anthropic", messages: kept } }, service),
  ).toEqual([
    { role: "user", content: "Add a button" },
    ...kept,
    { role: "user", content: "(You edited app.tsx.)\n\nNow red" },
    { role: "assistant", content: "Partly" },
    { role: "user", content: "Again" },
  ]);
  // A turn kept for another service goes back as its text.
  expect(
    history(chat, { a1: { service: "openai", messages: kept } }, service)[1],
  ).toEqual({ role: "assistant", content: "Done." });
});
