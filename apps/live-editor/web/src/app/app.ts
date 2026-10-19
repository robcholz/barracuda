/**
 * Live Editor's state and what the person can do with it. Pages read the state and call these
 * actions; every change emits `change`, and pages render again. The open app is built whenever its
 * sources change, and a successful build restarts it in the preview.
 */

import type {
  AppStorage,
  LogEntry,
  Question,
  RunManifest,
  RunState,
} from "../run/app-run";
import * as db from "../state/db";
import {
  activeChat,
  applyProposal,
  appId,
  createProject,
  DEVICES,
  newId,
  proposed,
  restore,
  snapshot,
  summarize,
  withChat,
  withFiles,
  type Attachment,
  type Chat,
  type DeviceId,
  type Files,
  type Project,
} from "../state/project";
import type { BuildReply, Problem } from "../workers/build";
import type { RunReply } from "../workers/run";
import { BLANK, examples } from "./templates";
import { setLang, t, type Lang } from "../i18n";
import { Builder, Runner } from "./workers";

export type Provider = "anthropic" | "openai";

export interface Settings {
  provider: Provider;
  anthropicKey: string;
  anthropicModel: string;
  openaiKey: string;
  openaiBaseUrl: string;
  openaiModel: string;
  /** Proposed changes wait in Changes until the person applies them. */
  review: boolean;
  /** The agent may tap the preview to check its own changes. */
  tap: boolean;
  /** The preview device a new app starts on. */
  device: DeviceId;
  /** The device portal's address, as the person last typed it. */
  portal: string;
}

export const DEFAULT_SETTINGS: Settings = {
  provider: "anthropic",
  anthropicKey: "",
  anthropicModel: "claude-opus-5-5",
  openaiKey: "",
  openaiBaseUrl: "",
  openaiModel: "",
  review: true,
  tap: true,
  device: "round",
  portal: "",
};

export type Route =
  | { page: "new" }
  | { page: "app"; id: string }
  | { page: "install"; id: string }
  | { page: "settings" };

export type ViewId = "preview" | "code" | "changes" | "console";
export type ThemeChoice = "light" | "dark" | "system";

export interface BuildState {
  status: "idle" | "building" | "ok" | "failed";
  problem: Problem | null;
  warnings: Problem[];
  /** The last successful build, which the preview runs. */
  last: Extract<BuildReply, { ok: true }> | null;
  /** Which sources it built: the person's, or with the agent's proposal. */
  of: "before" | "after";
}

export interface RunView {
  state: RunState | "idle";
  reason?: string;
  logs: LogEntry[];
  question: Question | null;
  depth: number;
}

export interface Toast {
  tone: "success" | "error";
  title: string;
  text?: string;
}

/** What the agent does for the app: the agent module implements it. */
export interface Agent {
  send(text: string, attachments: Attachment[], replyTo?: string): void;
  stop(): void;
  readonly running: boolean;
}

const remember = (key: string) => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};
const keep = (key: string, value: string) => {
  try {
    localStorage.setItem(key, value);
  } catch {
    // a private window keeps nothing; the choice holds for this visit
  }
};

const MAX_LOGS = 500;

export class App extends EventTarget {
  settings: Settings = { ...DEFAULT_SETTINGS };
  projects = new Map<string, Project>();
  route: Route = { page: "new" };
  theme: ThemeChoice = (remember("le-theme") as ThemeChoice | null) ?? "system";
  sidebar: "open" | "closed" | null = remember("le-sidebar") as
    "open" | "closed" | null;
  views: ViewId[] = ["preview"];
  flash: ViewId | null = null;
  file = "app.tsx";
  /** The line the code view should bring into view, with a flash. */
  jump: { file: string; line: number; at: number } | null = null;
  version: "before" | "after" = "after";
  build: BuildState = {
    status: "idle",
    problem: null,
    warnings: [],
    last: null,
    of: "before",
  };
  run: RunView = { state: "idle", logs: [], question: null, depth: 0 };
  toast: Toast | null = null;
  agent: Agent | null = null;
  /** The last picture the preview drew. */
  frame: {
    pixels: Uint8Array;
    width: number;
    height: number;
    scale: number;
  } | null = null;
  ready = false;

  readonly base: URL;
  private builder: Builder;
  private runner: Runner;
  private buildTimer?: ReturnType<typeof setTimeout>;
  private saveTimers = new Map<string, ReturnType<typeof setTimeout>>();
  private toastTimer?: ReturnType<typeof setTimeout>;
  private flashTimer?: ReturnType<typeof setTimeout>;
  private building = 0;
  private framesListeners = new Set<() => void>();
  /** Waiters for the next build to finish, for the agent's build tool. */
  private buildWaiters: ((reply: BuildReply) => void)[] = [];

  constructor(base: URL) {
    super();
    this.base = base;
    this.builder = new Builder(new URL("workers/build.js", base));
    this.runner = new Runner(new URL("workers/run.js", base), (reply) =>
      this.heard(reply),
    );
  }

  emit() {
    this.dispatchEvent(new Event("change"));
  }

  /** Loads what this browser kept. */
  async load() {
    const [settings, projects] = await Promise.all([
      db.get<Settings>("settings", "settings"),
      db.all<Project>("projects"),
    ]);
    this.settings = { ...DEFAULT_SETTINGS, ...settings };
    for (const project of projects) this.projects.set(project.id, project);
    this.ready = true;
    this.emit();
  }

  // ── Routes ──────────────────────────────────────────────────────────

  /** Follows the address bar's hash: `#/new`, `#/app/<id>`, `#/app/<id>/install`, `#/settings`. */
  navigate(hash: string) {
    const parts = hash.replace(/^#\/?/, "").split("/");
    let route: Route = { page: "new" };
    if (parts[0] === "app" && parts[1] && this.projects.has(parts[1]))
      route =
        parts[2] === "install"
          ? { page: "install", id: parts[1] }
          : { page: "app", id: parts[1] };
    else if (parts[0] === "settings") route = { page: "settings" };
    const before = this.openId();
    this.route = route;
    const after = this.openId();
    if (after && after !== before) this.open(after);
    if (!after && before) this.close();
    this.emit();
  }

  /** The app the page is about: the one being edited or installed, or for settings the last open. */
  openId(): string | null {
    if (this.route.page === "app" || this.route.page === "install")
      return this.route.id;
    if (this.route.page === "settings") return this.lastOpen;
    return null;
  }

  private lastOpen: string | null = remember("le-last");

  project(): Project | undefined {
    const id = this.openId();
    return id ? this.projects.get(id) : undefined;
  }

  /** Apps, the most recently changed first. */
  apps(): Project[] {
    return [...this.projects.values()].sort((a, b) => b.at - a.at);
  }

  private open(id: string) {
    this.lastOpen = id;
    keep("le-last", id);
    const project = this.projects.get(id);
    this.views = ["preview"];
    this.file = "app.tsx" in (project?.files ?? {}) ? "app.tsx" : "main.lua";
    this.version = "after";
    this.build = {
      status: "idle",
      problem: null,
      warnings: [],
      last: null,
      of: "before",
    };
    this.run = { state: "idle", logs: [], question: null, depth: 0 };
    this.frame = null;
    this.runner.lifecycle("stop");
    this.scheduleBuild(0);
  }

  private close() {
    this.runner.lifecycle("stop");
    this.run = { state: "idle", logs: [], question: null, depth: 0 };
  }

  // ── Theme and layout ────────────────────────────────────────────────

  setTheme(choice: ThemeChoice) {
    this.theme = choice;
    keep("le-theme", choice);
    this.emit();
  }

  setLang(choice: Lang) {
    setLang(choice);
    this.emit();
  }

  setSidebar(choice: "open" | "closed") {
    this.sidebar = choice;
    keep("le-sidebar", choice);
    this.emit();
  }

  /** Opens a view on the left, next to the chat, or flashes its bar when it is open already. */
  show(view: ViewId) {
    if (this.views.includes(view)) {
      this.flash = view;
      clearTimeout(this.flashTimer);
      this.flashTimer = setTimeout(() => {
        this.flash = null;
        this.emit();
      }, 900);
    } else this.views = [view, ...this.views];
    this.emit();
  }

  toggleView(view: ViewId) {
    this.views = this.views.includes(view)
      ? this.views.filter((v) => v !== view)
      : [view, ...this.views];
    this.emit();
  }

  closeView(view: ViewId) {
    this.views = this.views.filter((v) => v !== view);
    this.emit();
  }

  /** Opens a file in the code view, at a line when one is given. */
  openFile(file: string, line?: number) {
    this.file = file;
    if (line) this.jump = { file, line, at: Date.now() };
    this.show("code");
  }

  notify(toast: Toast) {
    this.toast = toast;
    clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => {
      this.toast = null;
      this.emit();
    }, 5000);
    this.emit();
  }

  dismissToast() {
    this.toast = null;
    this.emit();
  }

  // ── Settings ────────────────────────────────────────────────────────

  async saveSettings(settings: Settings) {
    this.settings = settings;
    await db.put("settings", "settings", settings);
    this.emit();
  }

  // ── Apps ────────────────────────────────────────────────────────────

  /** Creates an app from a template (`blank` or an example's ID) and opens it. */
  async create(
    template: string,
    description: string,
    device: DeviceId,
  ): Promise<Project> {
    const files =
      template === "blank" ? BLANK : (await examples(this.base))[template];
    const id = appId(description || template, this.projects.keys());
    const project = createProject(id, files, device);
    this.projects.set(id, project);
    await db.put("projects", id, project);
    location.hash = `#/app/${id}`;
    // Follow at once, so the agent can start on the app before the hashchange arrives.
    this.navigate(location.hash);
    return project;
  }

  async deleteApp(id: string) {
    this.projects.delete(id);
    await db.remove("projects", id);
    await db.remove("transcripts", id);
    if (this.openId() === id) location.hash = "#/new";
    this.emit();
  }

  /** Replaces the open project and keeps it, soon after the last change. */
  update(project: Project) {
    this.projects.set(project.id, project);
    clearTimeout(this.saveTimers.get(project.id));
    this.saveTimers.set(
      project.id,
      setTimeout(() => {
        this.saveTimers.delete(project.id);
        void db.put("projects", project.id, this.projects.get(project.id));
      }, 300),
    );
    this.emit();
  }

  /** The person edits a file in the code view. */
  edit(name: string, text: string) {
    const project = this.project();
    if (!project || project.files[name] === text) return;
    let next = withFiles(project, { ...project.files, [name]: text });
    // The conversation notes the edit once, so the agent knows to read the file again.
    const chat = activeChat(next);
    const last = chat.items.at(-1);
    const note = `You edited ${name}`;
    if (chat.items.length && !(last?.kind === "note" && last.text === note))
      next = withChat(next, {
        ...chat,
        items: [
          ...chat.items,
          { kind: "note", id: newId("note"), text: note, at: Date.now() },
        ],
      });
    this.update(next);
    this.scheduleBuild();
  }

  /** Adds a file, or answers why it cannot be added. */
  addFile(name: string): string | null {
    const project = this.project();
    if (!project) return t.noAppOpen;
    const problem = fileNameProblem(name, project.files);
    if (problem) return problem;
    this.update(withFiles(project, { ...project.files, [name]: "" }));
    this.openFile(name);
    return null;
  }

  renameFile(from: string, to: string): string | null {
    const project = this.project();
    if (!project) return t.noAppOpen;
    if (from === to) return null;
    const problem = fileNameProblem(to, project.files);
    if (problem) return problem;
    const files: Files = {};
    for (const [name, text] of Object.entries(project.files))
      files[name === from ? to : name] = text;
    this.update(withFiles(project, files));
    this.file = to;
    this.scheduleBuild();
    return null;
  }

  deleteFile(name: string) {
    const project = this.project();
    if (!project) return;
    const files = { ...project.files };
    delete files[name];
    this.update(withFiles(project, files));
    this.file = "app.tsx" in files ? "app.tsx" : "main.lua";
    this.scheduleBuild();
  }

  /** Changes manifest fields from the settings page. */
  setManifest(fields: {
    name: string;
    version: string;
    shape: "round" | "rect";
  }) {
    const project = this.project();
    if (!project) return;
    const manifest = JSON.parse(project.files["app.json"]) as Record<
      string,
      unknown
    > & { view?: Record<string, unknown> };
    manifest.name = fields.name;
    manifest.version = fields.version;
    if (manifest.view) manifest.view.shape = fields.shape;
    else if ("app.tsx" in project.files)
      manifest.view = { shape: fields.shape };
    this.update(
      withFiles(project, {
        ...project.files,
        "app.json": `${JSON.stringify(manifest, null, 2)}\n`,
      }),
    );
    this.scheduleBuild();
  }

  restoreVersion(n: number) {
    const project = this.project();
    if (!project) return;
    const version = project.versions.find((v) => v.n === n);
    this.update(restore(project, n));
    this.notify({
      tone: "success",
      title: t.restored(n, t.stored(version?.title ?? "")),
    });
    this.scheduleBuild(0);
  }

  setDevice(device: DeviceId) {
    const project = this.project();
    if (!project) return;
    this.update({ ...project, device });
    this.restartApp();
  }

  // ── Proposals ───────────────────────────────────────────────────────

  applyChanges() {
    const project = this.project();
    if (!project?.proposal) return;
    const turn = project.proposal.turn;
    let next = applyProposal(project);
    next = this.markChanges(next, turn, "applied");
    this.update(next);
    this.version = "after";
    this.views = this.views.filter((v) => v !== "changes");
    this.notify({ tone: "success", title: t.changesApplied });
    this.scheduleBuild(0);
  }

  discardChanges() {
    const project = this.project();
    if (!project?.proposal) return;
    const turn = project.proposal.turn;
    this.update(
      this.markChanges({ ...project, proposal: null }, turn, "discarded"),
    );
    this.version = "after";
    this.scheduleBuild(0);
  }

  /** Marks the changes card of the agent turn `turn`. */
  markChanges(
    project: Project,
    turn: string,
    state: "applied" | "review" | "discarded",
  ): Project {
    const chats = project.chats.map((chat) => ({
      ...chat,
      items: chat.items.map((item) =>
        item.kind === "agent" && item.id === turn && item.changes
          ? { ...item, changes: { ...item.changes, state } }
          : item,
      ),
    }));
    return { ...project, chats };
  }

  /** The files and line counts of the open proposal. */
  proposalSummary() {
    const project = this.project();
    return project?.proposal
      ? summarize(project.files, project.proposal.files)
      : [];
  }

  pickVersion(version: "before" | "after") {
    this.version = version;
    this.scheduleBuild(0);
    this.emit();
  }

  // ── Chats ───────────────────────────────────────────────────────────

  newChat() {
    const project = this.project();
    if (!project) return;
    const chat: Chat = {
      id: newId("chat"),
      title: "New chat",
      items: [],
      at: Date.now(),
    };
    this.update({ ...project, chats: [...project.chats, chat], chat: chat.id });
  }

  pickChat(id: string) {
    const project = this.project();
    if (project) this.update({ ...project, chat: id });
  }

  // ── Build and run ───────────────────────────────────────────────────

  /** The sources the preview runs: the person's, or with the agent's proposal while it is reviewed. */
  previewFiles(project: Project): { files: Files; of: "before" | "after" } {
    return project.proposal && this.version === "after"
      ? { files: proposed(project), of: "after" }
      : { files: project.files, of: "before" };
  }

  scheduleBuild(delay = 400) {
    clearTimeout(this.buildTimer);
    this.buildTimer = setTimeout(() => void this.rebuild(), delay);
  }

  /** Builds the preview's sources now and, when they build, runs them. */
  async rebuild(): Promise<BuildReply | null> {
    const project = this.project();
    if (!project) return null;
    const { files, of } = this.previewFiles(project);
    const ticket = ++this.building;
    this.build = { ...this.build, status: "building" };
    this.emit();
    const reply = await this.builder.build(files);
    for (const waiter of this.buildWaiters.splice(0)) waiter(reply);
    if (ticket !== this.building || this.openId() !== project.id) return reply;
    if (reply.ok) {
      this.build = {
        status: "ok",
        problem: null,
        warnings: reply.warnings,
        last: reply,
        of,
      };
      this.startRun();
    } else {
      this.build = {
        ...this.build,
        status: "failed",
        problem: reply.problem,
        warnings: [],
      };
      this.log({
        level: "error",
        text: `build failed · ${reply.problem.message}`,
        source: "system",
        at: Date.now(),
      });
    }
    this.emit();
    return reply;
  }

  /** Builds `files` for the agent without running them, or waits for the build already running. */
  async buildFiles(files: Files): Promise<BuildReply> {
    return this.builder.build(files);
  }

  private startRun() {
    const project = this.project();
    const last = this.build.last;
    if (!project || !last) return;
    const display = DEVICES[project.device].display;
    this.run = { ...this.run, question: null, depth: 0 };
    const scale = Math.min(
      3,
      Math.max(1, Math.round(globalThis.devicePixelRatio || 1)),
    );
    this.runner.start(
      last.files,
      last.manifest as unknown as RunManifest,
      display ? { ...display } : null,
      project.storage,
      scale,
    );
  }

  restartApp() {
    this.runner.lifecycle("stop");
    if (this.build.last) this.startRun();
    else this.scheduleBuild(0);
  }

  /** Empties what the app stored and starts it again. */
  resetStorage() {
    const project = this.project();
    if (!project) return;
    this.update({ ...project, storage: { kv: {}, files: {} } });
    this.restartApp();
    this.notify({ tone: "success", title: t.storageReset });
  }

  tap(x: number, y: number) {
    this.runner.tap(x, y);
  }

  lifecycle(event: "back" | "background" | "foreground" | "terminate") {
    this.runner.lifecycle(event);
  }

  answer(allowed: boolean) {
    this.runner.answer(allowed);
  }

  inspect() {
    return this.runner.inspect();
  }

  clearLog() {
    this.run = { ...this.run, logs: [] };
    this.emit();
  }

  onFrame(listener: () => void): () => void {
    this.framesListeners.add(listener);
    return () => this.framesListeners.delete(listener);
  }

  private log(entry: LogEntry) {
    this.run = {
      ...this.run,
      logs: [...this.run.logs, entry].slice(-MAX_LOGS),
    };
  }

  private heard(reply: RunReply) {
    switch (reply.type) {
      case "frame":
        this.frame = reply;
        for (const listener of this.framesListeners) listener();
        return;
      case "log":
        this.log(reply.entry);
        break;
      case "state":
        this.run = { ...this.run, state: reply.state, reason: reply.reason };
        if (reply.state === "ended") this.frame = null;
        break;
      case "ask":
        this.run = { ...this.run, question: reply.question };
        break;
      case "notice":
        this.notify({ tone: "success", title: reply.title, text: reply.body });
        return;
      case "navigation":
        this.run = { ...this.run, depth: reply.depth };
        break;
      case "storage": {
        const project = this.project();
        if (project) {
          this.update({ ...project, storage: reply.storage as AppStorage });
          return;
        }
        break;
      }
    }
    this.emit();
  }

  /** Takes a version of the current sources now, for example before the agent changes them. */
  keepVersion(title: string) {
    const project = this.project();
    if (project) this.update(snapshot(project, title));
  }
}

/** Why `name` cannot be a new file next to `files`, or `null`. */
export function fileNameProblem(name: string, files: Files): string | null {
  if (!/^[A-Za-z0-9_.-]{1,64}$/.test(name) || name === "." || name === "..")
    return t.badFileName;
  if (name in files) return t.alreadyFile(name);
  return null;
}
