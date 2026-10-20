/**
 * One run of an app in the preview: its entry program on Lua 5.4 (wasmoon) with the `app` and
 * `notify` packages from `boot.lua.ts`, and its view on the device's UI core (`wasm.ts`). The run
 * keeps the device's rules: one event stream, `app:active` first, the 1 s grace period after
 * `app:background` and `app:terminate`, and the storage limits of `app.kv` and `app.fs`.
 */

import type { LuaFactory } from "wasmoon";
import type { Module, Preview } from "../wasm";
import { BOOT } from "./boot.lua";

/** The display the preview device has; `null` is a device without one. */
export interface Display {
  width: number;
  height: number;
  round: boolean;
}

/** What an app keeps between runs: `app.kv` and `app.fs`. */
export interface AppStorage {
  kv: Record<string, unknown>;
  files: Record<string, string>;
}

/** A line in the console: the program's own output, or what the preview did. */
export interface LogEntry {
  level: "log" | "warn" | "error";
  text: string;
  /** `app` for the program's output, `system` for what the preview says about the run. */
  source: "app" | "system";
  at: number;
}

/** A `notify.ask` waiting for the person. */
export interface Question {
  title: string;
  body?: string;
  allow?: string;
  deny?: string;
}

/** Where a run is. */
export type RunState = "starting" | "active" | "background" | "ended";

/** What a run tells the page. */
export interface RunOutput {
  log(entry: LogEntry): void;
  frame(pixels: Uint8Array, width: number, height: number, scale: number): void;
  storage(storage: AppStorage): void;
  state(state: RunState, reason?: string): void;
  ask(question: Question | null): void;
  notice(title: string, body?: string): void;
  navigation(depth: number): void;
}

/** The manifest fields a run reads. */
export interface RunManifest {
  id: string;
  name: string;
  version: string;
  entry?: string;
  view?: { program: string; styles: string; fonts?: string[] };
  requires?: string[];
  uses?: string[];
  permissions?: string[];
}

export interface RunOptions {
  lua: LuaFactory;
  module: Module;
  files: ReadonlyMap<string, Uint8Array>;
  manifest: RunManifest;
  display: Display | null;
  storage: AppStorage;
  out: RunOutput;
  /** Physical pixels per view pixel. */
  scale?: number;
  /** How long the program may run without returning to `app.events()`. */
  limitMs?: number;
}

/** The device's grace period after `app:background` and `app:terminate`. */
export const GRACE_MS = 1000;
const MIN_TICK_MS = 100;
const MAX_KEY_BYTES = 64;
const MAX_STORE_BYTES = 16 * 1024;
const MAX_FILE_BYTES = 64 * 1024;
const MAX_DEPTH = 8;

const bytes = (text: string) => new TextEncoder().encode(text).length;
const reply = (value: unknown) => JSON.stringify({ v: value ?? null });
const refuse = (message: string) => JSON.stringify({ e: message });

/** A path in the app's file tree: relative, at most 8 names deep, no `.` or `..`. */
function checkPath(path: unknown, dir = false): string | null {
  if (typeof path !== "string") return null;
  const trimmed = dir ? path.replace(/\/$/, "") : path;
  if (trimmed === "") return dir ? "" : null;
  const names = trimmed.split("/");
  if (names.length > MAX_DEPTH) return null;
  if (names.some((name) => name === "" || name === "." || name === ".."))
    return null;
  return trimmed;
}

export class AppRun {
  private state: RunState = "starting";
  private step?: (payload?: string) => string;
  private preview: Preview | null = null;
  private queue: unknown[][] = [];
  private waiting = false;
  private asking = false;
  private stepping = false;
  private terminating = false;
  private timers: ReturnType<typeof setInterval>[] = [];
  private grace?: ReturnType<typeof setTimeout>;
  private depth = 0;
  private notices = 0;
  private readonly started = Date.now();
  private readonly entry: string;
  private readonly scale: number;

  private constructor(private readonly options: RunOptions) {
    this.entry = options.manifest.entry ?? "main.lua";
    this.scale = options.scale ?? 1;
  }

  /** Starts the app: mounts its view, runs its program to its first `app.events()`, then makes it active. */
  static async start(options: RunOptions): Promise<AppRun> {
    const run = new AppRun(options);
    await run.boot();
    return run;
  }

  private log(
    level: LogEntry["level"],
    text: string,
    source: LogEntry["source"] = "system",
  ) {
    this.options.out.log({ level, text, source, at: Date.now() });
  }

  private async boot() {
    const { manifest, files, display, module } = this.options;
    const source = files.get(this.entry);
    if (source === undefined)
      return this.end(`the app has no ${this.entry}`, "error");
    if (manifest.view) {
      if (!display) {
        this.log(
          "warn",
          `${manifest.name} has a view; this device has no screen`,
        );
        return this.end("this device has no screen");
      }
      const file = (name: string) => {
        const found = files.get(name);
        if (!found) throw new Error(`the app has no ${name}`);
        return found;
      };
      try {
        this.preview = module.open(
          {
            program: file(manifest.view.program),
            styles: file(manifest.view.styles),
            fonts: (manifest.view.fonts ?? []).map(file),
          },
          display.width,
          display.height,
        );
      } catch (error) {
        return this.end(
          error instanceof Error ? error.message : String(error),
          "error",
        );
      }
    }
    const engine = await this.options.lua.createEngine({
      injectObjects: false,
      enableProxy: false,
    });
    const named = [
      ...(manifest.requires ?? []),
      ...(manifest.uses ?? []),
      ...(manifest.permissions ?? []),
    ].map((name) => name.split("@")[0]);
    const describe = JSON.stringify({
      id: manifest.id,
      name: manifest.name,
      version: manifest.version,
      view: !!manifest.view,
      packages: named.filter((name) => name === "notify"),
      named,
    });
    engine.global.set("__host", (op: string, a?: unknown, b?: unknown) =>
      op === "manifest" ? describe : this.host(op, a, b),
    );
    engine.global.set("__source", new TextDecoder().decode(source));
    engine.global.set("__entry", this.entry);
    engine.global.set("__limit", this.options.limitMs ?? 5000);
    this.step = (await engine.doString(BOOT)) as (payload?: string) => string;
    this.resume(undefined);
    if (this.state === "ended") return;
    if (this.preview) {
      this.become("active");
      this.deliver("app:active");
      this.startFrames();
    } else {
      // Headless apps never come to the foreground: they run as services.
      this.become("active");
    }
  }

  private become(state: RunState, reason?: string) {
    this.state = state;
    this.options.out.state(state, reason);
  }

  /** The run's state. */
  get current(): RunState {
    return this.state;
  }

  /** The running view, for tests and the agent: its contract and its text. */
  get view(): Preview | null {
    return this.preview;
  }

  private startFrames() {
    const preview = this.preview;
    if (!preview) return;
    const rate = preview.tickRate() || 60;
    const paint = () => {
      if (this.state !== "active" || !this.preview) return;
      preview.frame();
      const pixels = preview.draw(this.scale);
      if (pixels)
        this.options.out.frame(
          pixels,
          preview.width,
          preview.height,
          this.scale,
        );
    };
    paint();
    this.timers.push(setInterval(paint, 1000 / rate));
  }

  /** Queues an event and runs the program while it waits for one. */
  private deliver(name: string, ...args: unknown[]) {
    if (this.state === "ended") return;
    this.queue.push([args.length + 1, name, ...args]);
    this.pump();
  }

  private pump() {
    while (
      this.waiting &&
      !this.stepping &&
      this.queue.length &&
      this.state !== "ended"
    ) {
      const event = this.queue.shift();
      this.waiting = false;
      this.resume(JSON.stringify(event));
    }
  }

  private resume(payload: string | undefined) {
    if (!this.step || this.state === "ended") return;
    this.stepping = true;
    let answer: unknown[];
    try {
      answer = JSON.parse(this.step(payload)) as unknown[];
    } catch (error) {
      answer = [
        "error",
        error instanceof Error ? error.message : String(error),
      ];
    } finally {
      this.stepping = false;
    }
    const [kind, detail] = answer as [string, unknown];
    if (kind === "wait") {
      this.waiting = true;
      if (this.terminating) return this.end("ended after app:terminate");
      this.pump();
    } else if (kind === "ask") {
      this.asking = true;
      this.options.out.ask(detail as Question);
    } else if (kind === "done") {
      this.end(`${this.entry} returned`);
    } else {
      this.log("error", String(detail), "app");
      this.end(`${this.entry} stopped with an error`, "error");
    }
  }

  /** The person answers the `notify.ask` on screen. */
  answer(allowed: boolean) {
    if (!this.asking) return;
    this.asking = false;
    this.options.out.ask(null);
    this.resume(JSON.stringify(allowed));
  }

  /** A tap on the screen at (`x`, `y`) in view pixels: the handler it selects becomes an event. */
  tap(x: number, y: number) {
    if (this.state !== "active" || !this.preview || this.asking) return;
    const call = this.preview.press(x, y);
    if (!call) return;
    this.log(
      "log",
      `${call.handler}(${call.args.map((arg) => JSON.stringify(arg)).join(", ")})`,
    );
    this.deliver(call.handler, ...call.args);
  }

  /** The back swipe: `app:back` while the app reports a page deeper than its first. */
  back(): boolean {
    if (this.state !== "active" || this.depth === 0) return false;
    this.depth = 0;
    this.options.out.navigation(0);
    this.deliver("app:back");
    return true;
  }

  /** Sends the app to the background: it must be back in `app.events()` within the grace period. */
  background() {
    if (this.state !== "active" || !this.preview) return;
    this.become("background");
    this.deliver("app:inactive");
    this.deliver("app:background");
    this.expect("app:background");
  }

  /** Brings it back: `app:foreground`, then `app:active`. */
  foreground() {
    if (this.state !== "background") return;
    this.become("active");
    this.deliver("app:foreground");
    this.deliver("app:active");
  }

  /** Asks the app to exit, and cancels it if it is not done within the grace period. */
  terminate() {
    if (this.state === "ended") return;
    this.terminating = true;
    this.deliver("app:terminate");
    this.expect("app:terminate");
  }

  private expect(event: string) {
    clearTimeout(this.grace);
    this.grace = setTimeout(() => {
      if (this.state === "ended") return;
      if (!this.waiting || this.queue.length || this.asking)
        this.end(
          `cancelled: not back in app.events() within 1 s of ${event}`,
          "error",
        );
    }, GRACE_MS);
  }

  /** Stops the run at once, as a crash or a closed preview would. */
  stop() {
    this.end("stopped");
  }

  private end(reason: string, level: "log" | "error" = "log") {
    if (this.state === "ended") return;
    for (const timer of this.timers) clearInterval(timer);
    clearTimeout(this.grace);
    this.timers = [];
    this.preview = null;
    this.step = undefined;
    if (this.asking) this.options.out.ask(null);
    this.asking = false;
    this.log(level === "error" ? "error" : "log", reason);
    this.become("ended", reason);
  }

  private save() {
    this.options.out.storage(structuredClone(this.options.storage));
  }

  /** The host function `boot.lua.ts` calls: every answer is `{"v": value}` or `{"e": message}`. */
  private host(op: string, a: unknown, b: unknown): string | number {
    const { storage, display, manifest } = this.options;
    switch (op) {
      case "now":
        return Date.now() - this.started;
      case "print":
        this.log("log", String(a), "app");
        return reply(null);
      case "device":
        return reply({
          api: 1,
          board: "live-editor-preview",
          display: display ? { ...display } : null,
          touch: !!display,
          capabilities: { notify: 2 },
        });
      case "view.set": {
        if (!this.preview) return refuse("the view is closed");
        const problem = this.preview.set(
          String(a),
          b === undefined ? null : JSON.parse(String(b)),
        );
        return problem ? refuse(problem) : reply(true);
      }
      case "view.close":
        if (this.preview) this.end(`${manifest.name} closed its view`);
        return reply(null);
      case "navigation": {
        this.depth = Math.max(0, Number(a) || 0);
        this.options.out.navigation(this.depth);
        return reply(true);
      }
      case "every": {
        const ms = Number(a);
        if (!Number.isFinite(ms) || ms < MIN_TICK_MS)
          return refuse("every is at least 100 ms");
        this.timers.push(
          setInterval(() => {
            if (this.state === "active" && !this.terminating)
              this.deliver("app:tick");
          }, ms),
        );
        return reply(true);
      }
      case "kv.get": {
        const key = String(a);
        return reply(Object.hasOwn(storage.kv, key) ? storage.kv[key] : null);
      }
      case "kv.set": {
        const key = String(a);
        if (bytes(key) < 1 || bytes(key) > MAX_KEY_BYTES)
          return refuse("keys are 1–64 bytes");
        const value = JSON.parse(String(b)) as unknown;
        const next = { ...storage.kv };
        if (value === null) delete next[key];
        else next[key] = value;
        if (bytes(JSON.stringify(next)) > MAX_STORE_BYTES)
          return refuse("the store holds at most 16 KiB of JSON");
        storage.kv = next;
        this.save();
        return reply(true);
      }
      case "kv.delete":
        delete storage.kv[String(a)];
        this.save();
        return reply(true);
      case "kv.keys":
        return reply(Object.keys(storage.kv).sort());
      case "notify.post":
        this.notices += 1;
        this.options.out.notice(
          String(a),
          b === undefined || b === null ? undefined : String(b),
        );
        this.log("log", `notify.post: ${a}${b ? ` · ${b}` : ""}`);
        return reply(this.notices);
      default:
        return this.files(op, a, b);
    }
  }

  private files(op: string, a: unknown, b: unknown): string {
    const { storage } = this.options;
    const dir = op === "fs.list" || op === "fs.mkdir";
    const path = checkPath(a, dir);
    if (path === null)
      return refuse(
        "paths are relative, at most 8 names deep, with no . or ..",
      );
    const dirs = (name: string) =>
      Object.keys(storage.files).filter((file) => file.startsWith(`${name}/`));
    switch (op) {
      case "fs.read":
        return Object.hasOwn(storage.files, path)
          ? reply(storage.files[path])
          : refuse(`${path}: no such file`);
      case "fs.write":
      case "fs.append": {
        const text =
          (op === "fs.append" ? (storage.files[path] ?? "") : "") + String(b);
        if (bytes(text) > MAX_FILE_BYTES)
          return refuse("files are at most 64 KiB");
        storage.files[path] = text;
        this.save();
        return reply(true);
      }
      case "fs.exists":
        return reply(
          Object.hasOwn(storage.files, path) || dirs(path).length > 0,
        );
      case "fs.remove":
        if (!Object.hasOwn(storage.files, path))
          return refuse(`${path}: no such file`);
        delete storage.files[path];
        this.save();
        return reply(true);
      case "fs.mkdir":
        // Directories exist through the files in them; an empty one keeps a marker.
        if (!dirs(path).length) storage.files[`${path}/.keep`] = "";
        this.save();
        return reply(true);
      case "fs.list": {
        const prefix = path ? `${path}/` : "";
        const names = new Set<string>();
        for (const file of Object.keys(storage.files)) {
          if (!file.startsWith(prefix)) continue;
          const rest = file.slice(prefix.length);
          const [head, ...tail] = rest.split("/");
          if (head === ".keep") continue;
          names.add(tail.length ? `${head}/` : head);
        }
        return reply([...names].sort());
      }
      default:
        return refuse(`unknown host call ${op}`);
    }
  }
}
