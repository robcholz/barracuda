import { afterEach, beforeEach, expect, test } from "bun:test";
import { installBrowser } from "../../../../captive-portal/resources/web/tests/browser";
import {
  append,
  cut,
  type Frame,
  load,
  MAX_SESSIONS,
  PREFIX,
  prune,
  save,
  trim,
} from "../history";

let browser: ReturnType<typeof installBrowser>;
beforeEach(() => {
  browser = installBrowser();
});
afterEach(async () => {
  await browser.close();
});

const delta = (id: string, text: string): Frame => [
  "e",
  "message.event",
  { message_id: id, type: "output_delta", payload: { text } },
];

test("a stream's consecutive text deltas are kept as one frame", () => {
  const frames: Frame[] = [];
  append(frames, ["u", "a~web-in-1", "hi", null, []]);
  append(frames, ["e", "message.start", { message_id: "a~web-1" }]);
  append(frames, delta("a~web-1", "Hel"));
  append(frames, delta("a~web-1", "lo\n"));
  append(frames, ["e", "message.delta", { message_id: "a~web-2", delta: "x" }]);
  append(frames, ["e", "message.delta", { message_id: "a~web-2", delta: "y" }]);
  // another message, or another stream of the same one, starts a frame of its own
  append(frames, delta("a~web-3", "!"));
  append(frames, [
    "e",
    "message.event",
    { message_id: "a~web-3", type: "reasoning_delta", payload: { text: "?" } },
  ]);
  expect(frames).toEqual([
    ["u", "a~web-in-1", "hi", null, []],
    ["e", "message.start", { message_id: "a~web-1" }],
    delta("a~web-1", "Hello\n"),
    ["e", "message.delta", { message_id: "a~web-2", delta: "xy" }],
    delta("a~web-3", "!"),
    [
      "e",
      "message.event",
      {
        message_id: "a~web-3",
        type: "reasoning_delta",
        payload: { text: "?" },
      },
    ],
  ]);
});

test("a long copy loses its oldest whole turns; a message taken back cuts the copy there", () => {
  const turn = (n: number): Frame[] => [
    ["u", `a~web-in-${n}`, `q${n}`, null, []],
    delta(`a~web-${n}`, "x".repeat(40)),
  ];
  const frames = [...turn(1), ...turn(2), ...turn(3)];
  const ids = () =>
    frames.map((frame) => (frame[0] === "u" ? frame[1] : frame[2].message_id));
  const one = turn(1).reduce((sum, f) => sum + JSON.stringify(f).length, 0);
  trim(frames, one * 2);
  expect(ids()).toEqual(["a~web-in-2", "a~web-2", "a~web-in-3", "a~web-3"]);
  cut(frames, "a~web-in-3");
  expect(ids()).toEqual(["a~web-in-2", "a~web-2"]);
});

test("copies are saved per session, the latest kept, and those the device no longer lists removed", () => {
  for (let n = 0; n <= MAX_SESSIONS; n++)
    save(`session-${n}`, [["u", "a~web-in-1", `q${n}`, null, []]], n);
  expect(load("session-0")).toEqual([]);
  expect(load(`session-${MAX_SESSIONS}`)).toEqual([
    ["u", "a~web-in-1", `q${MAX_SESSIONS}`, null, []],
  ]);
  prune(new Set([`session-${MAX_SESSIONS}`]));
  const left = Object.keys(browser.window.localStorage).filter((key) =>
    key.startsWith(PREFIX),
  );
  expect(left).toEqual([`${PREFIX}session-${MAX_SESSIONS}`]);
  // an unreadable copy is no copy
  browser.window.localStorage.setItem(`${PREFIX}session-9`, "{");
  expect(load("session-9")).toEqual([]);
});
