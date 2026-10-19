/**
 * The agent: one turn per message the person sends. Each turn streams the model's responses into
 * the chat, runs the tools it calls against the open app, and leaves its file changes as a
 * proposal, applied at once unless the person reviews changes first.
 *
 * The conversation goes back to the model service exactly as it went out: each turn keeps the
 * messages it sent and received in the service's own format, and later turns only append.
 */

import type { Agent, App } from "../app/app";
import * as db from "../state/db";
import {
  DEVICES,
  activeChat,
  newId,
  proposed,
  summarize,
  withChat,
  type Attachment,
  type Chat,
  type ChatItem,
  type Project,
  type Step,
} from "../state/project";
import { anthropic } from "./anthropic";
import { openai } from "./openai";
import { SYSTEM } from "./prompt";
import type { Service } from "./service";
import { t } from "../i18n";
import {
  TOOLS,
  checkInput,
  numbered,
  replaceOnce,
  type ToolOutput,
} from "./tools";

type AgentItem = Extract<ChatItem, { kind: "agent" }>;
type UserItem = Extract<ChatItem, { kind: "user" }>;

/** What a finished turn kept for the next request: the service, and its messages after the user's. */
export interface Transcript {
  service: Service["id"];
  messages: unknown[];
}

const MAX_STEPS = 40;
const FILE_NAME = /^[A-Za-z0-9_.-]{1,64}$/;

/** The transcripts are kept beside the chat, keyed by turn, so the chat stays plain data to render. */
type Transcripts = Record<string, Transcript>;

/** The model service the settings name, or why there is none. */
export function serviceFor(app: App): Service | string {
  const s = app.settings;
  if (s.provider === "anthropic") {
    if (!s.anthropicKey.trim()) return t.needsAnthropicKey;
    return anthropic(
      s.anthropicKey.trim(),
      s.anthropicModel.trim() || "claude-opus-5-5",
    );
  }
  if (!s.openaiBaseUrl.trim() || !s.openaiModel.trim()) return t.needsOpenai;
  return openai(
    s.openaiBaseUrl.trim(),
    s.openaiKey.trim(),
    s.openaiModel.trim(),
  );
}

/** The requests's history: every earlier turn, as the service received it, then nothing else. */
export function history(
  chat: Chat,
  transcripts: Transcripts,
  service: Service,
): unknown[] {
  const messages: unknown[] = [];
  let notes: string[] = [];
  for (const item of chat.items) {
    if (item.kind === "note") {
      notes.push(item.text);
      continue;
    }
    if (item.kind === "user") {
      const text = notes.length
        ? `${notes.map((note) => `(${note}.)`).join(" ")}\n\n${item.text}`
        : item.text;
      notes = [];
      messages.push(service.user(text, item.attachments));
      continue;
    }
    const kept = transcripts[item.id];
    if (kept && kept.service === service.id) messages.push(...kept.messages);
    else {
      // A turn from another service, or one that kept nothing: its reply as text.
      const reply = item.steps
        .map((step) => step.text)
        .filter(Boolean)
        .join("\n\n");
      messages.push({ role: "assistant", content: reply || "(no reply)" });
    }
  }
  return messages;
}

/** Encodes RGBA pixels as a PNG, scaled to `width` × `height`, as base64. */
async function png(
  pixels: Uint8Array,
  width: number,
  height: number,
  scale: number,
  out: { width: number; height: number },
): Promise<string> {
  const source = new OffscreenCanvas(width * scale, height * scale);
  source
    .getContext("2d")
    ?.putImageData(
      new ImageData(
        new Uint8ClampedArray(pixels),
        width * scale,
        height * scale,
      ),
      0,
      0,
    );
  const target = new OffscreenCanvas(out.width, out.height);
  target.getContext("2d")?.drawImage(source, 0, 0, out.width, out.height);
  const blob = await target.convertToBlob({ type: "image/png" });
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000)
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

class Controller implements Agent {
  private abort: AbortController | null = null;
  running = false;

  constructor(private readonly app: App) {}

  stop() {
    this.abort?.abort();
  }

  send(text: string, attachments: Attachment[], replyTo?: string) {
    const project = this.app.project();
    if (!project || this.running) return;
    void this.turn(project.id, text, attachments, replyTo);
  }

  // ── The open app, always read fresh: the person keeps working while the agent does ──

  private project(id: string): Project {
    const project = this.app.projects.get(id);
    if (!project) throw new Error("the app was deleted");
    return project;
  }

  private change(id: string, edit: (project: Project) => Project) {
    this.app.update(edit(this.project(id)));
  }

  private editItem(
    id: string,
    chatId: string,
    turn: string,
    edit: (item: AgentItem) => AgentItem,
  ) {
    this.change(id, (project) => {
      const chat = project.chats.find((c) => c.id === chatId);
      if (!chat) return project;
      return withChat(project, {
        ...chat,
        items: chat.items.map((item) =>
          item.kind === "agent" && item.id === turn ? edit(item) : item,
        ),
      });
    });
  }

  private async turn(
    id: string,
    text: string,
    attachments: Attachment[],
    replyTo?: string,
  ) {
    const app = this.app;
    const start = this.project(id);
    const chat = activeChat(start);
    const user: UserItem = {
      kind: "user",
      id: newId("user"),
      text,
      attachments,
      at: Date.now(),
      replyTo,
    };
    const turn = newId("turn");
    const item: AgentItem = {
      kind: "agent",
      id: turn,
      at: Date.now(),
      steps: [],
      status: "running",
    };
    const title =
      chat.items.length === 0
        ? text.replace(/\s+/g, " ").slice(0, 48)
        : chat.title;
    this.change(id, (project) =>
      withChat(project, { ...chat, title, items: [...chat.items, user, item] }),
    );

    const service = serviceFor(app);
    if (typeof service === "string") {
      this.editItem(id, chat.id, turn, (it) => ({
        ...it,
        status: "error",
        error: service,
      }));
      return;
    }
    this.running = true;
    this.abort = new AbortController();
    const signal = this.abort.signal;
    app.emit();

    const transcripts = { ...(await loadTranscripts(id)) };
    const sent = history(
      { ...chat, items: [...chat.items, user] },
      transcripts,
      service,
    );
    const mine: unknown[] = [];
    const tools = TOOLS.filter(
      (tool) => tool.name !== "tap" || app.settings.tap,
    );
    let flush = 0;
    let steps: Step[] = [];
    const show = (force = false) => {
      const now = Date.now();
      if (!force && now - flush < 80) return;
      flush = now;
      const shown = steps.map((step) => ({ ...step, tools: [...step.tools] }));
      this.editItem(id, chat.id, turn, (it) => ({ ...it, steps: shown }));
    };
    let ended: Pick<AgentItem, "status" | "error"> = { status: "done" };

    try {
      for (let n = 0; n < MAX_STEPS; n += 1) {
        const step: Step = { text: "", tools: [] };
        steps = [...steps, step];
        let thinkingFrom = 0;
        const result = await service.step(
          SYSTEM,
          tools,
          [...sent, ...mine],
          signal,
          {
            thinking: (delta) => {
              thinkingFrom ||= Date.now();
              step.thinking = {
                text: (step.thinking?.text ?? "") + delta,
                ms: Date.now() - thinkingFrom,
              };
              show();
            },
            text: (delta) => {
              step.text += delta;
              show();
            },
            tool: (callId, name) => {
              if (!step.tools.some((call) => call.id === callId))
                step.tools.push({
                  id: callId,
                  name,
                  input: {},
                  status: "running",
                });
              show();
            },
          },
        );
        step.text = result.text;
        if (result.thinking || step.thinking)
          step.thinking = { text: result.thinking, ms: step.thinking?.ms ?? 0 };
        mine.push(result.message);
        if (result.stop === "refused") {
          ended = {
            status: "error",
            error: t.declined(result.detail),
          };
          break;
        }
        if (result.stop === "truncated") {
          ended = {
            status: "error",
            error: t.truncated,
          };
          break;
        }
        if (result.stop === "end") break;
        step.tools = result.calls.map((call) => ({
          id: call.id,
          name: call.name,
          input: (call.input && typeof call.input === "object"
            ? call.input
            : {}) as Record<string, unknown>,
          status: "running" as const,
        }));
        show(true);
        const outputs: { id: string; output: ToolOutput }[] = [];
        for (const [index, call] of result.calls.entries()) {
          const output = signal.aborted
            ? { text: "Stopped by the person", isError: true }
            : await this.run(id, turn, text, call.name, call.input);
          outputs.push({ id: call.id, output });
          step.tools[index] = {
            ...step.tools[index],
            output: output.text,
            isError: output.isError,
            status: output.isError ? "error" : "done",
          };
          show(true);
        }
        mine.push(...service.results(outputs));
        if (signal.aborted) break;
      }
      if (signal.aborted) ended = { status: "stopped" };
    } catch (error) {
      ended = signal.aborted
        ? { status: "stopped" }
        : { status: "error", error: service.describe(error) };
    }

    // Only a turn that finished is replayed as it went; any other goes back as its text.
    if (ended.status === "done")
      transcripts[turn] = { service: service.id, messages: mine };
    await saveTranscripts(id, transcripts);
    show(true);
    this.finish(id, chat.id, turn, ended);
    this.running = false;
    this.abort = null;
    app.emit();
  }

  /** Ends the turn: its changes card, and the proposal applied or left for review. */
  private finish(
    id: string,
    chatId: string,
    turn: string,
    ended: Pick<AgentItem, "status" | "error">,
  ) {
    const app = this.app;
    const project = this.project(id);
    const files =
      project.proposal?.turn === turn
        ? summarize(project.files, project.proposal.files)
        : [];
    if (!files.length) {
      if (project.proposal?.turn === turn)
        this.change(id, (p) => ({ ...p, proposal: null }));
      this.editItem(id, chatId, turn, (it) => ({ ...it, ...ended }));
      return;
    }
    const review = app.settings.review;
    this.editItem(id, chatId, turn, (it) => ({
      ...it,
      ...ended,
      changes: { files, state: review ? "review" : "applied" },
    }));
    if (app.openId() !== id) return;
    if (review) app.show("changes");
    else app.applyChanges();
  }

  /** Runs one tool call against the app `id`. */
  private async run(
    id: string,
    turn: string,
    request: string,
    name: string,
    input: unknown,
  ): Promise<ToolOutput> {
    const problem = checkInput(name, input);
    if (problem) return { text: `Invalid input: ${problem}`, isError: true };
    const args = input as Record<string, string>;
    const app = this.app;
    const files = () => proposed(this.project(id));
    const propose = (name: string, text: string | null) =>
      this.change(id, (project) => {
        const proposal = project.proposal ?? {
          turn,
          title: request.replace(/\s+/g, " ").slice(0, 60),
          files: {},
        };
        const next = { ...proposal.files, [name]: text };
        // A file back to what it was is no change.
        if (
          text === project.files[name] ||
          (text === null && !(name in project.files))
        )
          delete next[name];
        return { ...project, proposal: { ...proposal, turn, files: next } };
      });
    const open = app.openId() === id;
    switch (name) {
      case "list_files": {
        const all = files();
        return {
          text: Object.keys(all)
            .sort()
            .map((file) => `${file} (${all[file].split("\n").length} lines)`)
            .join("\n"),
        };
      }
      case "read_file": {
        const text = files()[args.path];
        return text === undefined
          ? { text: `There is no ${args.path}`, isError: true }
          : { text: numbered(text) };
      }
      case "write_file": {
        if (!FILE_NAME.test(args.path))
          return {
            text: "Use 1–64 letters, digits, dots, dashes or underscores",
            isError: true,
          };
        propose(args.path, args.content);
        return {
          text: `Wrote ${args.path} (${args.content.split("\n").length} lines)`,
        };
      }
      case "edit_file": {
        const text = files()[args.path];
        if (text === undefined)
          return { text: `There is no ${args.path}`, isError: true };
        const edited = replaceOnce(text, args.old_text, args.new_text);
        if (typeof edited !== "string")
          return { text: edited.error, isError: true };
        propose(args.path, edited);
        return { text: `Edited ${args.path}` };
      }
      case "delete_file": {
        if (!(args.path in files()))
          return { text: `There is no ${args.path}`, isError: true };
        propose(args.path, null);
        return { text: `Deleted ${args.path}` };
      }
      case "build": {
        const reply = open
          ? ((app.version = "after"), await app.rebuild())
          : await app.buildFiles(files());
        if (!reply) return { text: "No app is open", isError: true };
        if (!reply.ok) {
          const where = [
            reply.problem.file,
            reply.problem.line,
            reply.problem.column,
          ]
            .filter((part) => part !== undefined)
            .join(":");
          return {
            text: `Build failed${where ? ` at ${where}` : ""}: ${reply.problem.message}`,
            isError: true,
          };
        }
        const warnings = reply.warnings.map(
          (w) => `${w.file}:${w.line}: ${w.message}`,
        );
        return {
          text: `Built in ${reply.ms} ms.${open ? " The preview restarted with it." : ""}${warnings.length ? `\nWarnings:\n${warnings.join("\n")}` : ""}`,
        };
      }
      case "look":
        return open
          ? this.look(id)
          : { text: "The app is not open in the preview", isError: true };
      case "tap": {
        if (!app.settings.tap)
          return {
            text: "The person turned off tapping the preview",
            isError: true,
          };
        if (!open)
          return { text: "The app is not open in the preview", isError: true };
        app.tap(Number(args.x), Number(args.y));
        await wait(500);
        return this.look(id);
      }
      case "read_console": {
        const lines = app.run.logs
          .slice(-60)
          .map(
            (entry) =>
              `${entry.level} ${entry.source === "system" ? "[preview] " : ""}${entry.text}`,
          );
        return {
          text: lines.length ? lines.join("\n") : "The console is empty.",
        };
      }
      case "restart": {
        if (!open)
          return { text: "The app is not open in the preview", isError: true };
        app.restartApp();
        await wait(800);
        return { text: `Restarted. The app is ${app.run.state}.` };
      }
    }
    return { text: `Unknown tool ${name}`, isError: true };
  }

  private async look(id: string): Promise<ToolOutput> {
    const app = this.app;
    const project = this.project(id);
    const device = DEVICES[project.device];
    const seen = await app.inspect();
    const lines = [
      `The app is ${seen.state === "active" ? "running" : seen.state}${app.run.reason ? ` (${app.run.reason})` : ""} on ${device.label}.`,
    ];
    if (app.build.status === "failed")
      lines.push(
        "The last build failed; the preview runs the build before it.",
      );
    if (seen.texts.length)
      lines.push(
        `Text on screen: ${seen.texts.map((text) => JSON.stringify(text)).join(", ")}`,
      );
    if (seen.contract)
      lines.push(
        `Values: ${
          Object.entries(seen.contract.values)
            .map(([k, v]) => `${k}: ${v}`)
            .join(", ") || "none"
        }`,
        `Handlers: ${
          Object.entries(seen.contract.handlers)
            .map(([k, v]) => `${k}(${v.join(", ")})`)
            .join(", ") || "none"
        }`,
      );
    if (app.run.question)
      lines.push(`A notify.ask card is open: ${app.run.question.title}`);
    const frame = app.frame;
    if (!frame || !device.display) return { text: lines.join("\n") };
    lines.push(
      `The screenshot is ${frame.width}×${frame.height} view pixels; tap coordinates use the same pixels.`,
    );
    const image = await png(
      frame.pixels,
      frame.width,
      frame.height,
      frame.scale,
      { width: frame.width, height: frame.height },
    );
    return { text: lines.join("\n"), image };
  }
}

// The transcripts live in their own IndexedDB record per app: they can be large (screenshots).
async function loadTranscripts(id: string): Promise<Transcripts> {
  return (await db.get<Transcripts>("transcripts", id)) ?? {};
}
async function saveTranscripts(id: string, transcripts: Transcripts) {
  await db.put("transcripts", id, transcripts);
}

/** Gives the app its agent. */
export function attachAgent(app: App) {
  app.agent = new Controller(app);
}
