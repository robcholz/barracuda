/**
 * The run worker: one app run at a time, off the page's thread, so a program that spins cannot
 * stall the editor. The page sends a built container and the person's input; the worker answers
 * with frames, console lines and what the app stored.
 */

import { LuaFactory } from "wasmoon";
import {
  AppRun,
  type AppStorage,
  type Display,
  type RunManifest,
  type RunOutput,
} from "../run/app-run";
import { loadModule, type Module, type ViewContract } from "../wasm";

export type RunRequest =
  | {
      type: "start";
      files: Record<string, Uint8Array>;
      manifest: RunManifest;
      display: Display | null;
      storage: AppStorage;
      scale: number;
    }
  | { type: "tap"; x: number; y: number }
  | { type: "back" | "background" | "foreground" | "terminate" | "stop" }
  | { type: "answer"; allowed: boolean }
  | { type: "inspect"; id: number };

export type RunReply =
  | { type: "log"; entry: Parameters<RunOutput["log"]>[0] }
  | {
      type: "frame";
      pixels: Uint8Array;
      width: number;
      height: number;
      scale: number;
    }
  | { type: "storage"; storage: AppStorage }
  | { type: "state"; state: Parameters<RunOutput["state"]>[0]; reason?: string }
  | { type: "ask"; question: Parameters<RunOutput["ask"]>[0] }
  | { type: "notice"; title: string; body?: string }
  | { type: "navigation"; depth: number }
  | {
      type: "inspected";
      id: number;
      texts: string[];
      contract: ViewContract | null;
      state: string;
    };

const base = new URL("../", import.meta.url);
const post = (reply: RunReply, transfer: Transferable[] = []) =>
  (self as unknown as Worker).postMessage(reply, transfer);

let tools: Promise<{ module: Module; lua: LuaFactory }> | undefined;
async function load() {
  const module = await loadModule(new URL("live-editor.wasm", base));
  const names = (await (
    await fetch(new URL("fonts/index.json", base))
  ).json()) as string[];
  const fonts = await Promise.all(
    names.map(
      async (name) =>
        new Uint8Array(
          await (await fetch(new URL(`fonts/${name}`, base))).arrayBuffer(),
        ),
    ),
  );
  module.systemFonts(fonts);
  return { module, lua: new LuaFactory(new URL("lua.wasm", base).href) };
}

let run: AppRun | null = null;
let starting = 0;

const out: RunOutput = {
  log: (entry) => post({ type: "log", entry }),
  frame: (pixels, width, height, scale) => {
    const copy = pixels.slice();
    post({ type: "frame", pixels: copy, width, height, scale }, [copy.buffer]);
  },
  storage: (storage) => post({ type: "storage", storage }),
  state: (state, reason) => post({ type: "state", state, reason }),
  ask: (question) => post({ type: "ask", question }),
  notice: (title, body) => post({ type: "notice", title, body }),
  navigation: (depth) => post({ type: "navigation", depth }),
};

self.onmessage = async (event: MessageEvent<RunRequest>) => {
  const request = event.data;
  switch (request.type) {
    case "start": {
      run?.stop();
      run = null;
      const ticket = ++starting;
      tools ??= load();
      const { module, lua } = await tools;
      if (ticket !== starting) return;
      run = await AppRun.start({
        lua,
        module,
        files: new Map(Object.entries(request.files)),
        manifest: request.manifest,
        display: request.display,
        storage: request.storage,
        scale: request.scale,
        out,
      });
      return;
    }
    case "tap":
      return run?.tap(request.x, request.y);
    case "back":
      return run?.back();
    case "background":
      return run?.background();
    case "foreground":
      return run?.foreground();
    case "terminate":
      return run?.terminate();
    case "stop":
      run?.stop();
      run = null;
      return;
    case "answer":
      return run?.answer(request.allowed);
    case "inspect": {
      const view = run?.view ?? null;
      return post({
        type: "inspected",
        id: request.id,
        texts: view ? view.texts() : [],
        contract: view ? view.contract() : null,
        state: run?.current ?? "ended",
      });
    }
  }
};
