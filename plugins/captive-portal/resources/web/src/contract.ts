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
