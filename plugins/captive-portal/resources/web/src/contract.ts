/**
 * The portal's contracts, types only: the manifest the shell reads, the context it gives each
 * contributor module, and the live-figure module shape. Contributors import these types through
 * `ui/`; nothing here is emitted into any bundle.
 */

/** A label in both portal languages. */
export interface PortalText {
  zh: string;
  en: string;
}

/** Where an entry sits in the portal's navigation. */
export type WebGroup = "device" | "agent" | "channel";

/** One `/portal/entries.json` record, with asset URLs made absolute by the portal. */
export interface WebEntry {
  id: string;
  group: WebGroup;
  order: number;
  title: PortalText;
  summary: PortalText;
  /** Absolute icon URL: `.svg` is a monochrome mark drawn in `currentColor`, anything else an image. */
  icon: string | null;
  /** Absolute URL of a module exporting `figure` (see {@link FigureDefinition}). */
  figure: string | null;
  module: string;
}

export type Lang = "zh" | "en";

/** How an entry is doing, for the navigation and the overview: set up, needs a look, or not set up. */
export type EntryState = "ready" | "attention" | "off";

/** One entry's record in `GET /portal/status`: its state, a short label, and a machine value. */
export interface EntryStatus {
  state: EntryState;
  /** 「已连接」, 「已配置」; absent when the state alone says enough. */
  label?: PortalText;
  /** A machine value the shell shows in mono, for example the network name. */
  detail?: string;
}

/** A live state of the open page, shown in the top bar: Web chat's 已连接 or 已断开. */
export interface PageBadge {
  label: string;
  /** `live` is the signal chip; `lost` the destructive badge. */
  tone: "live" | "lost";
}

export interface Toast {
  kind: "success" | "error" | "info";
  title: string;
  body?: string;
  /** A short machine value shown after the title in mono, for example an HTTP status. */
  code?: string;
  action?: { label: string; run(): void };
}

export interface PortalContext {
  /** Aborted on navigation, language change and unload. */
  signal: AbortSignal;
  /** The module renders its own strings in this language; a change remounts the module. */
  lang: Lang;
  /** The shell's toast stack (bottom-right; errors stay until closed, others close after 4 s). */
  toast(toast: Toast): void;
  /** Go to another entry by ID, or `"overview"` for home. */
  navigate(id: string): void;
  /**
   * The latest `GET /portal/status` record for an entry (this page's own by default), or `null`
   * when the entry reports none or the portal could not read it.
   */
  status(id?: string): EntryStatus | null;
  /**
   * Reads the status again and redraws the navigation and overview with it. Call it after a save
   * succeeds; it resolves once the new status is shown (or the read failed), and never rejects.
   */
  refreshStatus(): Promise<void>;
  /**
   * Shows this page's live state in the top bar, or clears it with `null`. It replaces the device
   * badge while the page is open and goes away when the page closes. Absent in a shell that
   * predates it.
   */
  badge?(badge: PageBadge | null): void;
}

export type Cleanup = () => void;

export interface PortalModule {
  mount(
    root: HTMLElement,
    context: PortalContext,
  ): void | Cleanup | Promise<void | Cleanup>;
}

/** What a figure's mount receives: its stage (the figure's whole 400 × 320 frame) and its svg. */
export interface FigureTarget {
  stage: HTMLElement;
  svg: SVGSVGElement;
  /** A read-out the figure may name its state in; the portal does not show it. */
  read: { textContent: string | null };
}

export interface FigureHandle {
  destroy(): void;
  set?(value: number): void;
  /** Router-style figures: start or stop an ambient sweep (`<hl-figure scan="true">`). */
  scan?(on: boolean): void;
}

/**
 * A live figure, the object a Hairline source passes to `hairline(...)`. A contributor's figure
 * module exports it as `export const figure = { name, range, mount }` and uses the global `HL`.
 */
export interface FigureDefinition {
  name: string;
  means?: string;
  rules?: readonly number[];
  /** The figure's number at intensity 0, 0.5 and 1; the portal mounts it at `range[1]`. */
  range: readonly [number, number, number];
  /** The crop the figure is shown at, in its 400 × 320 frame; defaults to the portal's table or the whole frame. */
  view?: readonly [number, number, number, number];
  /** Where a pointer on the surrounding hover zone, but off the figure, is taken to be. */
  answer?: readonly [number, number];
  mount(target: FigureTarget, value: number): FigureHandle;
}
