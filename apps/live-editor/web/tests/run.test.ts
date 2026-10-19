// A run in the preview keeps the device's rules. These run once `bun apps/live-editor/web/build.ts`
// has built dist/live-editor.wasm.
import { expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { LuaFactory } from "wasmoon";
import {
  AppRun,
  type AppStorage,
  type LogEntry,
  type RunManifest,
  type RunOutput,
  type RunState,
} from "../src/run/app-run";
import { loadModule, type Module } from "../src/wasm";

const repository = resolve(import.meta.dir, "../../../..");
const wasm = resolve(import.meta.dir, "../dist/live-editor.wasm");
const COUNTER = join(
  repository,
  "plugins/app-manager/crates/appkit/tests/fixtures/counter",
);
const built = existsSync(wasm);
const lua = new LuaFactory();
let loaded: Module | undefined;
const module = async () => (loaded ??= await loadModule(readFileSync(wasm)));

function counterFiles(program?: string) {
  const files = new Map<string, Uint8Array>();
  for (const name of readdirSync(COUNTER))
    files.set(name, new Uint8Array(readFileSync(join(COUNTER, name))));
  if (program) files.set("main.lua", new TextEncoder().encode(program));
  const manifest = JSON.parse(
    readFileSync(join(COUNTER, "app.json"), "utf8"),
  ) as RunManifest;
  return { files, manifest };
}

function recorder() {
  const logs: LogEntry[] = [];
  const states: RunState[] = [];
  const saved: AppStorage[] = [];
  let frames = 0;
  const out: RunOutput = {
    log: (entry) => logs.push(entry),
    frame: () => {
      frames += 1;
    },
    storage: (storage) => saved.push(storage),
    state: (state) => states.push(state),
    ask: () => {},
    notice: () => {},
    navigation: () => {},
  };
  return { out, logs, states, saved, frames: () => frames };
}

async function start(
  program?: string,
  storage: AppStorage = { kv: {}, files: {} },
  extra: Partial<RunManifest> = {},
) {
  const record = recorder();
  const { files, manifest } = counterFiles(program);
  Object.assign(manifest, extra);
  const run = await AppRun.start({
    lua,
    module: await module(),
    files,
    manifest,
    display: { width: 360, height: 360, round: true },
    storage,
    out: record.out,
    limitMs: 500,
  });
  return { run, ...record };
}

/** Taps until the view's text says `text`. */
function tapUntil(run: AppRun, text: string) {
  for (let y = 0; y < 360; y += 6)
    for (let x = 0; x < 360; x += 6) {
      const before = run.view?.texts().join(" ");
      run.tap(x, y);
      run.view?.frame();
      if (run.view?.texts().includes(text)) return true;
      if (run.view?.texts().join(" ") !== before) return false;
    }
  return false;
}

test.if(built)(
  "the counter runs: taps reach main.lua and its values reach the view",
  async () => {
    const { run, logs, states, frames } = await start();
    expect(states).toEqual(["active"]);
    expect(frames()).toBeGreaterThan(0);
    expect(run.view?.texts()).toContain("0");
    expect(tapUntil(run, "1")).toBe(true);
    expect(logs.some((entry) => entry.text === "increment(1)")).toBe(true);
    run.stop();
  },
);

test.if(built)(
  "app:background saves, and the saved state comes back on the next run",
  async () => {
    const first = await start();
    tapUntil(first.run, "1");
    first.run.background();
    expect(first.saved.at(-1)?.kv.state).toMatchObject({ count: 1 });
    first.run.terminate();
    expect(first.states.at(-1)).toBe("ended");

    const second = await start(undefined, first.saved.at(-1));
    expect(second.run.view?.texts()).toContain("1");
    second.run.stop();
  },
);

test.if(built)(
  "print, errors and view:set mistakes reach the console in the device's words",
  async () => {
    const { logs, states } = await start(`
local app = require("app")
local view = app.view()
print("hello", 1, nil, 2.5)
print(view:set("count", "three"))
print(view:set("nothing", 1))
print(app.kv.set("bad", { 1, x = 2 }))
print(pcall(require, "light"))
for event in app.events() do
  if event == "app:active" then error("boom") end
end`);
    const texts = logs
      .filter((entry) => entry.source === "app")
      .map((entry) => entry.text);
    expect(texts[0]).toBe("hello\t1\tnil\t2.5");
    expect(texts[1]).toBe("nil\t'count': expected an integer");
    expect(texts[2]).toBe("nil\tthe view has no value named 'nothing'");
    expect(texts[3]).toBe(
      "nil\tthe store keeps tables that are arrays or have string keys",
    );
    expect(texts[4]).toContain("module 'light' not found");
    expect(texts[5]).toContain("main.lua:10: boom");
    expect(states.at(-1)).toBe("ended");
  },
);

test.if(built)(
  "a program that never returns to app.events() is stopped",
  async () => {
    const { logs, states } = await start(`while true do end`);
    expect(states.at(-1)).toBe("ended");
    expect(
      logs.some((entry) =>
        entry.text.includes("without returning to app.events()"),
      ),
    ).toBe(true);
  },
);

test.if(built)(
  "a program still busy a second after app:terminate is cancelled",
  async () => {
    const { run, states, logs } = await start(
      `
local app = require("app")
local notify = require("notify")
for event in app.events() do
  if event == "app:terminate" then notify.ask("Save first?") end
end`,
      undefined,
      { requires: ["notify@2"] },
    );
    run.terminate();
    expect(states.at(-1)).toBe("active");
    await Bun.sleep(1100);
    expect(states.at(-1)).toBe("ended");
    expect(logs.at(-1)?.text).toBe(
      "cancelled: not back in app.events() within 1 s of app:terminate",
    );
  },
);
