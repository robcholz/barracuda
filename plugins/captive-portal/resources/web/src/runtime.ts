import type {
  Cleanup,
  EntryState,
  EntryStatus,
  Lang,
  PortalContext,
  PortalModule,
  PortalText,
  Toast,
  WebEntry,
  WebGroup,
} from "./contract";

export type {
  Cleanup,
  EntryState,
  EntryStatus,
  Lang,
  PortalContext,
  PortalModule,
  PortalText,
  Toast,
  WebEntry,
  WebGroup,
};

export const GROUPS: readonly WebGroup[] = ["device", "agent", "channel"];
const ID = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const SEGMENT = /^[a-zA-Z0-9_.-]+$/;

function text(value: unknown): PortalText | undefined {
  const record = value as Partial<PortalText> | null;
  return record &&
    typeof record === "object" &&
    typeof record.zh === "string" &&
    typeof record.en === "string"
    ? { zh: record.zh, en: record.en }
    : undefined;
}

/** An absolute asset URL inside `/portal/assets/<id>/`, with the portal's relative-path rules. */
function asset(id: string, value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const prefix = `/portal/assets/${id}/`;
  if (!value.startsWith(prefix)) return undefined;
  const relative = value.slice(prefix.length);
  if (
    !relative ||
    relative.length > 256 ||
    !relative
      .split("/")
      .every((part) => SEGMENT.test(part) && part !== "." && part !== "..")
  )
    return undefined;
  return value;
}

/** Validates `/portal/entries.json`. Any malformed record or foreign URL rejects the whole manifest. */
export function parseEntries(value: unknown): WebEntry[] {
  if (!Array.isArray(value)) throw new Error("Manifest is not an array");
  const ids = new Set<string>();
  return value.map((item) => {
    const id = item?.id;
    if (typeof id !== "string" || !ID.test(id) || id.length > 64)
      throw new Error("Manifest entry has an invalid ID");
    if (ids.has(id)) throw new Error(`Duplicate manifest entry ${id}`);
    ids.add(id);
    const title = text(item.title),
      summary = text(item.summary);
    const module = asset(id, item.module);
    const icon = item.icon === null ? null : asset(id, item.icon);
    const figure = item.figure === null ? null : asset(id, item.figure);
    if (
      !GROUPS.includes(item.group) ||
      !Number.isInteger(item.order) ||
      item.order < 0 ||
      item.order > 255 ||
      !title ||
      !summary ||
      !module ||
      icon === undefined ||
      figure === undefined
    )
      throw new Error(`Manifest entry ${id} is malformed`);
    return {
      id,
      group: item.group,
      order: item.order,
      title,
      summary,
      icon,
      figure,
      module,
    };
  });
}

const STATES: readonly EntryState[] = ["ready", "attention", "off"];
/** Longer details are not machine values the shell can show on one line. */
const DETAIL_MAX = 128;

/**
 * Validates `GET /portal/status` (`{"entries": {"<id>": {state, label?, detail?}}}`). A record with
 * an unknown state, a malformed label or detail, or an invalid ID is left out; anything but an
 * object with an `entries` object rejects the whole reply.
 */
export function parseStatus(value: unknown): Map<string, EntryStatus> {
  const entries = (value as { entries?: unknown } | null)?.entries;
  if (
    !value ||
    typeof value !== "object" ||
    Array.isArray(value) ||
    !entries ||
    typeof entries !== "object" ||
    Array.isArray(entries)
  )
    throw new Error("Status is not an object of entries");
  const out = new Map<string, EntryStatus>();
  for (const [id, item] of Object.entries(entries as Record<string, unknown>)) {
    const record = item as Record<string, unknown> | null;
    if (!ID.test(id) || id.length > 64 || !record || typeof record !== "object")
      continue;
    const state = record.state as EntryState;
    const label = record.label === undefined ? null : text(record.label);
    const detail = record.detail;
    if (
      !STATES.includes(state) ||
      label === undefined ||
      (detail !== undefined &&
        (typeof detail !== "string" || !detail || detail.length > DETAIL_MAX))
    )
      continue;
    const status: EntryStatus = { state };
    if (label) status.label = label;
    if (typeof detail === "string") status.detail = detail;
    out.set(id, status);
  }
  return out;
}

/** The portal's navigation order: device, agent, channel; then `order`; then ID. */
export function sortEntries(entries: readonly WebEntry[]): WebEntry[] {
  return [...entries].sort(
    (a, b) =>
      GROUPS.indexOf(a.group) - GROUPS.indexOf(b.group) ||
      a.order - b.order ||
      (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );
}

/** What the shell lends each mount; the session adds the signal and silences it once aborted. */
export interface SessionHost {
  lang: Lang;
  toast(toast: Toast): void;
  navigate(id: string): void;
  status(id: string): EntryStatus | null;
  refreshStatus(): Promise<void>;
}

export interface OpenOptions {
  load?: (url: string) => Promise<unknown>;
  /** Runs once the module is imported, before `mount`: the shell swaps its loading state for the root. */
  onImported?: () => void;
}

function runCleanup(cleanup: Cleanup | undefined) {
  try {
    cleanup?.();
  } catch (error) {
    console.error("Module cleanup failed", error);
  }
}

/** Each navigation gets an isolated root, context and abort signal. Imports stay external to the shell bundle. */
export class ModuleSession {
  private controller?: AbortController;
  private cleanup?: Cleanup;

  close(): void {
    this.controller?.abort();
    this.controller = undefined;
    const cleanup = this.cleanup;
    this.cleanup = undefined;
    runCleanup(cleanup);
  }

  /** Resolves `true` once mounted, `false` when a newer navigation superseded it; rejects on import or mount failure. */
  async open(
    entry: WebEntry,
    root: HTMLElement,
    host: SessionHost,
    { load = (url) => import(url), onImported }: OpenOptions = {},
  ): Promise<boolean> {
    this.close();
    const controller = new AbortController();
    this.controller = controller;
    const { signal } = controller;
    const context: PortalContext = Object.freeze({
      signal,
      lang: host.lang,
      toast: (toast: Toast) => {
        if (!signal.aborted) host.toast(toast);
      },
      navigate: (id: string) => {
        if (!signal.aborted) host.navigate(id);
      },
      status: (id: string = entry.id) => host.status(id),
      refreshStatus: () =>
        signal.aborted ? Promise.resolve() : host.refreshStatus(),
    });
    const module = (await load(entry.module)) as Partial<PortalModule> | null;
    if (signal.aborted) return false;
    if (!module || typeof module.mount !== "function")
      throw new Error(`${entry.module} does not export mount()`);
    onImported?.();
    const cleanup = (await module.mount(root, context)) || undefined;
    if (cleanup !== undefined && typeof cleanup !== "function")
      throw new Error(`${entry.module} returned an invalid cleanup`);
    if (signal.aborted) {
      runCleanup(cleanup);
      return false;
    }
    this.cleanup = cleanup;
    return true;
  }
}
