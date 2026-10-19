// Sending a package through the device portal's window.
import { expect, test } from "bun:test";
import {
  Handoff,
  portalOrigin,
  portalUrl,
  READY_MS,
  type Host,
} from "../src/install/handoff";

test("the address the person types becomes the portal's origin", () => {
  expect(portalOrigin("192.168.1.42:8787")).toBe("http://192.168.1.42:8787");
  expect(portalOrigin(" http://barracuda.local/ ")).toBe(
    "http://barracuda.local",
  );
  expect(portalOrigin("https://10.0.0.2")).toBe("https://10.0.0.2");
  expect(portalOrigin("10.0.0.2/portal")).toBeNull();
  expect(portalOrigin("")).toBeNull();
  expect(portalUrl("http://10.0.0.2")).toBe("http://10.0.0.2/portal/#files");
});

/** A host whose window and clock the test drives. */
function fakeHost(opens = true) {
  const posted: { message: Record<string, unknown>; origin: string }[] = [];
  const portal = {
    closed: false,
    close: () => (portal.closed = true),
    postMessage: (message: Record<string, unknown>, origin: string) =>
      posted.push({ message, origin }),
  };
  let listener: ((event: MessageEvent) => void) | null = null;
  const timers: (() => void)[] = [];
  let opened = "";
  const host: Host = {
    open: (url) => (
      (opened = url),
      opens ? (portal as unknown as Window) : null
    ),
    listen: (handler) => ((listener = handler), () => (listener = null)),
    setTimeout: (handler) => timers.push(handler),
    clearTimeout: () => {},
    setInterval: () => 0,
    clearInterval: () => {},
  };
  const say = (
    data: unknown,
    from: unknown = portal,
    origin = "http://10.0.0.2",
  ) => listener?.({ data, source: from, origin } as unknown as MessageEvent);
  return {
    host,
    portal,
    posted,
    say,
    timers,
    opened: () => opened,
    listening: () => listener !== null,
  };
}

const offer = {
  id: "counter",
  name: "Counter",
  version: "0.1.0",
  files: new Map([
    ["app.json", new TextEncoder().encode('{"id":"counter"}')],
    ["main.lua", new TextEncoder().encode("print(1)")],
  ]),
};

test("the app's files go to the portal window once it is ready, and its steps come back", () => {
  const fake = fakeHost();
  const phases: string[] = [];
  const handoff = new Handoff(fake.host, () => phases.push(handoff.phase));
  handoff.start("http://10.0.0.2", offer);
  expect(fake.opened()).toBe("http://10.0.0.2/portal/#files");
  // Strangers, and the right window at another origin, are not the device.
  fake.say({ type: "barracuda/ready", v: 1 }, {});
  fake.say(
    { type: "barracuda/ready", v: 1 },
    fake.portal,
    "http://evil.example",
  );
  expect(fake.posted).toEqual([]);
  fake.say({ type: "barracuda/ready", v: 1 });
  expect(fake.posted[0].origin).toBe("http://10.0.0.2");
  expect(fake.posted[0].message).toMatchObject({
    type: "barracuda/app",
    v: 1,
    id: "counter",
    name: "Counter",
    version: "0.1.0",
  });
  const files = fake.posted[0].message.files as Record<string, ArrayBuffer>;
  expect(Object.keys(files)).toEqual(["app.json", "main.lua"]);
  expect(new TextDecoder().decode(files["main.lua"])).toBe("print(1)");
  for (const state of ["review", "writing", "installed"])
    fake.say({ type: "barracuda/status", v: 1, id: "counter", state });
  expect(phases).toEqual(["opening", "ready", "ready", "writing", "done"]);
  expect(fake.listening()).toBe(false);
});

test("a failed write keeps the window listening; a refusal, a silent portal and a blocked window fail", () => {
  const fake = fakeHost();
  const handoff = new Handoff(fake.host, () => {});
  handoff.start("http://10.0.0.2", offer);
  fake.say({ type: "barracuda/ready", v: 1 });
  fake.say({
    type: "barracuda/status",
    v: 1,
    id: "counter",
    state: "failed",
    message: "存储卡已移除",
  });
  expect(handoff.failure).toEqual({ kind: "device", message: "存储卡已移除" });
  expect(fake.listening()).toBe(true);
  fake.say({ type: "barracuda/status", v: 1, id: "counter", state: "refused" });
  expect(handoff.failure).toEqual({ kind: "refused" });
  expect(fake.listening()).toBe(false);

  const silent = fakeHost();
  const waiting = new Handoff(silent.host, () => {});
  waiting.start("http://10.0.0.2", offer);
  expect(READY_MS).toBeGreaterThan(0);
  silent.timers[0]();
  expect(waiting.failure).toEqual({ kind: "silent" });

  const blocked = new Handoff(fakeHost(false).host, () => {});
  blocked.start("http://10.0.0.2", offer);
  expect(blocked.failure).toEqual({ kind: "blocked" });
});
