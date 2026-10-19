/**
 * An app in Live Editor: its sources, the versions it went through, its conversations with the
 * agent, the changes the agent proposes, and what the preview's runs stored. Plain data, kept in
 * IndexedDB; every change goes through the functions here.
 */

import type { AppStorage } from "../run/app-run";

/** Source files by name. */
export type Files = Record<string, string>;

/** A snapshot of the sources, taken when changes are applied. */
export interface Version {
  n: number;
  title: string;
  at: number;
  files: Files;
}

/** The preview devices: Barracuda displays and a device without one. */
export const DEVICES = {
  round: {
    label: "Round 360 · touch",
    size: "360 × 360",
    display: { width: 360, height: 360, round: true },
  },
  square: {
    label: "Square 320 · touch",
    size: "320 × 320",
    display: { width: 320, height: 320, round: false },
  },
  none: { label: "No display", size: "headless", display: null },
} as const;
export type DeviceId = keyof typeof DEVICES;

/** A file the person attached to a message. */
export interface Attachment {
  name: string;
  type: string;
  size: number;
  /** Text files are sent as text. */
  text?: string;
  /** Images are sent as base64. */
  data?: string;
}

/** A tool the agent called, and what came back. */
export interface ToolCall {
  id: string;
  name: string;
  input: Record<string, unknown>;
  output?: string;
  isError?: boolean;
  status: "running" | "done" | "error";
}

/** One model response within an agent turn: what it thought, said and called. */
export interface Step {
  thinking?: { text: string; signature?: string; ms: number };
  text: string;
  tools: ToolCall[];
}

/** The files an agent turn changed, with line counts, and where the change stands. */
export interface ChangeSummary {
  files: { name: string; add: number; del: number }[];
  state: "applied" | "review" | "discarded";
}

export type ChatItem =
  | {
      kind: "user";
      id: string;
      text: string;
      attachments: Attachment[];
      at: number;
      replyTo?: string;
    }
  | {
      kind: "agent";
      id: string;
      at: number;
      steps: Step[];
      status: "running" | "done" | "stopped" | "error";
      error?: string;
      changes?: ChangeSummary;
    }
  | { kind: "note"; id: string; text: string; at: number };

export interface Chat {
  id: string;
  title: string;
  items: ChatItem[];
  at: number;
}

/** Changes the agent proposed and the person has not applied yet: `null` deletes a file. */
export interface Proposal {
  turn: string;
  title: string;
  files: Record<string, string | null>;
}

export interface Project {
  id: string;
  name: string;
  files: Files;
  versions: Version[];
  chats: Chat[];
  chat: string;
  proposal: Proposal | null;
  storage: AppStorage;
  device: DeviceId;
  at: number;
}

/** The files every app keeps: the manifest and its entry program; a view app also its view. */
export function required(files: Files): string[] {
  const names = ["app.json", entryOf(files)];
  if ("app.tsx" in files) names.push("app.tsx", "app.d.ts", "pocket.json");
  return names;
}

/** The manifest, read leniently: the build reports what is wrong with it. */
export function manifestOf(files: Files): {
  id?: string;
  name?: string;
  version?: string;
  entry?: string;
  view?: { shape?: string };
} {
  try {
    const manifest = JSON.parse(files["app.json"] ?? "{}") as unknown;
    return manifest && typeof manifest === "object" ? manifest : {};
  } catch {
    return {};
  }
}

export function entryOf(files: Files): string {
  const entry = manifestOf(files).entry;
  return typeof entry === "string" ? entry : "main.lua";
}

let counter = 0;
/** A short ID, unique in this browser. */
export function newId(prefix: string): string {
  counter += 1;
  return `${prefix}-${Date.now().toString(36)}${counter.toString(36)}${Math.random().toString(36).slice(2, 6)}`;
}

/** An app ID from a description: 1–32 of a-z, 0-9 and -, not one of `taken`. */
export function appId(description: string, taken: Iterable<string>): string {
  const words = description
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9\s-]/g, " ")
    .split(/[\s-]+/)
    .filter(
      (word) =>
        word &&
        !["a", "an", "the", "that", "with", "and", "to", "of", "for"].includes(
          word,
        ),
    );
  let base =
    words.slice(0, 3).join("-").slice(0, 24).replace(/-+$/, "") || "app";
  if (!/^[a-z]/.test(base)) base = `app-${base}`.slice(0, 24);
  const used = new Set(taken);
  if (!used.has(base)) return base;
  for (let n = 2; ; n += 1)
    if (!used.has(`${base}-${n}`)) return `${base}-${n}`;
}

/** A new app from template sources, with its first version. */
export function createProject(
  id: string,
  files: Files,
  device: DeviceId,
  now = Date.now(),
): Project {
  const manifest = JSON.parse(files["app.json"]) as Record<string, unknown>;
  manifest.id = id;
  const sources = {
    ...files,
    "app.json": `${JSON.stringify(manifest, null, 2)}\n`,
  };
  const chat = { id: newId("chat"), title: "New chat", items: [], at: now };
  return {
    id,
    name: String(manifest.name ?? id),
    files: sources,
    versions: [{ n: 1, title: "First version", at: now, files: sources }],
    chats: [chat],
    chat: chat.id,
    proposal: null,
    storage: { kv: {}, files: {} },
    device:
      "app.tsx" in files ? (device === "none" ? "round" : device) : "none",
    at: now,
  };
}

/** The sources with the proposal applied: what the agent sees and the preview's After runs. */
export function proposed(project: Project): Files {
  const files = { ...project.files };
  for (const [name, text] of Object.entries(project.proposal?.files ?? {})) {
    if (text === null) delete files[name];
    else files[name] = text;
  }
  return files;
}

const same = (a: Files, b: Files) => {
  const names = Object.keys(a);
  return (
    names.length === Object.keys(b).length &&
    names.every((name) => a[name] === b[name])
  );
};

/** The version the sources are at, or `null` once they were edited since. */
export function currentVersion(project: Project): Version | null {
  return (
    [...project.versions]
      .reverse()
      .find((version) => same(version.files, project.files)) ?? null
  );
}

/** Takes a version of the sources as they are now, unless one has them already. */
export function snapshot(
  project: Project,
  title: string,
  now = Date.now(),
): Project {
  if (currentVersion(project)) return project;
  const n = Math.max(0, ...project.versions.map((version) => version.n)) + 1;
  return {
    ...project,
    versions: [
      ...project.versions,
      { n, title, at: now, files: project.files },
    ],
  };
}

/** Sets the sources, following the manifest's name. */
export function withFiles(
  project: Project,
  files: Files,
  now = Date.now(),
): Project {
  const name = manifestOf(files).name;
  return {
    ...project,
    files,
    name: typeof name === "string" && name ? name : project.name,
    at: now,
  };
}

/** Applies the proposal and keeps the result as a version titled after it. */
export function applyProposal(project: Project, now = Date.now()): Project {
  if (!project.proposal) return project;
  const title = project.proposal.title;
  const applied = {
    ...withFiles(project, proposed(project), now),
    proposal: null,
  };
  return snapshot(applied, title, now);
}

/** Restores a version. Edits not in any version are kept as one first, and newer versions stay. */
export function restore(
  project: Project,
  n: number,
  now = Date.now(),
): Project {
  const version = project.versions.find((v) => v.n === n);
  if (!version) return project;
  const kept = snapshot(project, "Your edits", now);
  return { ...withFiles(kept, version.files, now), proposal: null };
}

/** Counts the lines a change adds and removes. */
export function lineCounts(
  before: string | undefined,
  after: string | null | undefined,
) {
  const a = before === undefined ? [] : before.split("\n");
  const b = after === undefined || after === null ? [] : after.split("\n");
  // Lines in common, by the longest common subsequence.
  const rows = a.length + 1;
  const cols = b.length + 1;
  const table = new Uint32Array(rows * cols);
  for (let i = a.length - 1; i >= 0; i -= 1)
    for (let j = b.length - 1; j >= 0; j -= 1)
      table[i * cols + j] =
        a[i] === b[j]
          ? table[(i + 1) * cols + j + 1] + 1
          : Math.max(table[(i + 1) * cols + j], table[i * cols + j + 1]);
  const common = table[0];
  return { add: b.length - common, del: a.length - common };
}

/** The files a proposal changes, with their line counts. */
export function summarize(
  base: Files,
  files: Record<string, string | null>,
): ChangeSummary["files"] {
  return Object.entries(files)
    .filter(
      ([name, text]) =>
        base[name] !== text && !(text === null && !(name in base)),
    )
    .map(([name, text]) => ({ name, ...lineCounts(base[name], text) }))
    .sort((x, y) => x.name.localeCompare(y.name));
}

/** The chat being shown. */
export function activeChat(project: Project): Chat {
  return (
    project.chats.find((chat) => chat.id === project.chat) ?? project.chats[0]
  );
}

/** Replaces one chat. */
export function withChat(project: Project, chat: Chat): Project {
  return {
    ...project,
    chats: project.chats.map((c) => (c.id === chat.id ? chat : c)),
  };
}
