/**
 * The live figures' host, ported from the design's canvas host (live/build.py → hl-live.js):
 * the Hairline kernel as the global `HL`, and `<hl-figure name="…">`, one live figure per element.
 * Built-in names are `board`, `riffle` and `plug`; any other name is a manifest entry ID whose
 * `figure` module is imported once (cached) and must export `figure`.
 */
import HL from "./kernel.js";
import { figure as board } from "./board.js";
import { figure as riffle } from "./riffle.js";
import { figure as plug } from "./plug.js";
import type { FigureDefinition, FigureHandle } from "../contract";

type Box = readonly [number, number, number, number];

const BUILT_IN: Record<string, FigureDefinition> = { board, riffle, plug };

// each figure's crop: the box the static file shows, so a live figure is the same size as the picture it replaces
const VIEW: Record<string, Box> = {
  board: [22.8, 40.1, 348.2, 278.6],
  socket: [33.2, 45.8, 333.5, 266.8],
  loupe: [20, 19.8, 359.9, 288],
  router: [46.2, 43.1, 302, 241.6],
  laptop: [4.1, 24.1, 355.7, 284.6],
  riffle: [66.2, 58.6, 269.4, 215.6],
  plug: [75, 32.7, 322.5, 258],
};

// Where the pointer on a figure's zone (data-hl-zone: a whole module tile) but off the figure is taken to be:
// a point on the part that answers, in the figure's own 400 × 320 frame. data-hl-at on an element inside the
// zone names its own point (a channel row raises its card).
const ANSWER: Record<string, readonly [number, number]> = {
  board: [155, 182],
  socket: [266, 174],
  loupe: [225, 168],
  router: [330, 110],
  laptop: [200, 170],
  riffle: [220, 119],
};

const STYLE =
  "hl-figure{display:block;position:relative;width:100%;aspect-ratio:5/4;background:var(--figure-plate)}" +
  "hl-figure>[data-hairline]{position:absolute;aspect-ratio:auto;--hairline-plate:var(--figure-plate);--hairline-hi:var(--figure-hi);" +
  "--hairline-edge:var(--figure-edge);--hairline-mid:var(--figure-mid);--hairline-lo:var(--figure-lo)}" +
  // the board's two LEDs, the only colour in a figure: power in the signal lime, activity in the destructive red
  "hl-figure .dot.led-g{fill:var(--signal)}hl-figure .dot.led-r{fill:var(--red-900)}hl-figure .dot.led-r.off{fill:var(--figure-lo)}";

type Loader = (url: string) => Promise<unknown>;
const importModule: Loader = (url) => import(url);
let resolve: (name: string) => string | null = () => null;
let loader: Loader = importModule;
const modules = new Map<string, Promise<FigureDefinition>>();

/**
 * Tells the host where a non-built-in figure name's module lives (the shell maps entry IDs to
 * manifest URLs), and optionally how to import it (tests).
 */
export function setFigureSource(
  source: (name: string) => string | null,
  load: Loader = importModule,
) {
  resolve = source;
  loader = load;
}

function isFigure(value: unknown): value is FigureDefinition {
  const figure = value as Partial<FigureDefinition> | null;
  return (
    !!figure &&
    typeof figure.name === "string" &&
    typeof figure.mount === "function" &&
    Array.isArray(figure.range) &&
    figure.range.length === 3
  );
}

/** Imports a contributor figure module once; a failed import is forgotten so a later mount can retry. */
export function loadFigure(
  url: string,
  load: Loader = loader,
): Promise<FigureDefinition> {
  let pending = modules.get(url);
  if (!pending) {
    pending = load(url).then((module) => {
      const figure = (module as { figure?: unknown } | null)?.figure;
      if (!isFigure(figure)) throw new Error(`${url} does not export figure`);
      return figure;
    });
    pending.catch(() => modules.delete(url));
    modules.set(url, pending);
  }
  return pending;
}

const styled = new WeakSet<Document>();

/** Publishes the kernel as the global `HL` and defines `<hl-figure>` in this window (once). */
export function defineFigureElement(win: Window & typeof globalThis = window) {
  (win as unknown as { HL: typeof HL }).HL = HL;
  (globalThis as unknown as { HL: typeof HL }).HL = HL;
  if (win.customElements.get("hl-figure")) return;

  class HLFigure extends win.HTMLElement {
    private handle: FigureHandle | null = null;
    private off: (() => void) | null = null;
    private generation = 0;

    static get observedAttributes() {
      return ["name", "scan"];
    }
    connectedCallback() {
      this.mount();
    }
    disconnectedCallback() {
      this.unmount();
    }
    attributeChangedCallback(attribute: string) {
      if (!this.isConnected) return;
      if (attribute === "scan")
        this.handle?.scan?.(this.getAttribute("scan") === "true");
      else {
        this.unmount();
        this.mount();
      }
    }

    private mount() {
      const name = this.getAttribute("name");
      if (!name || this.handle) return;
      const generation = ++this.generation;
      const builtIn = BUILT_IN[name];
      if (builtIn) return this.start(name, builtIn);
      const url = resolve(name);
      if (!url) return;
      loadFigure(url).then(
        (figure) => {
          if (generation === this.generation && this.isConnected)
            this.start(name, figure);
        },
        (error) => console.error("Figure load failed", error),
      );
    }

    private start(name: string, figure: FigureDefinition) {
      // the kernel's loop needs a real layout engine; without one (tests, very old browsers) the plate stays empty
      // (the kernel reads these as globals)
      if (
        typeof globalThis.IntersectionObserver !== "function" ||
        typeof globalThis.matchMedia !== "function" ||
        this.handle
      )
        return;
      HL.inject(win.document);
      if (!styled.has(win.document)) {
        styled.add(win.document);
        const style = win.document.createElement("style");
        style.textContent = STYLE;
        win.document.head.append(style);
      }
      if (this.hasAttribute("aria-label")) {
        if (!this.hasAttribute("role")) this.setAttribute("role", "img");
      } else this.setAttribute("aria-hidden", "true");
      // the stage is the figure's whole 400 × 320 frame, so the kernel's pointer maps right; this element shows the crop
      const [x, y, w, h] = figure.view ??
        VIEW[figure.name] ??
        VIEW[name] ?? [0, 0, 400, 320];
      const stage = win.document.createElement("div");
      stage.setAttribute("data-hairline", figure.name);
      stage.style.cssText = `left:${(-x / w) * 100}%;top:${(-y / h) * 100}%;width:${(400 / w) * 100}%;height:${(320 / h) * 100}%`;
      const svg = HL.mk(
        "svg",
        { viewBox: "0 0 400 320", "aria-hidden": "true" },
        stage,
      ) as SVGSVGElement;
      this.append(stage);
      try {
        this.handle = figure.mount(
          { stage, svg, read: { textContent: "" } },
          figure.range[1],
        );
      } catch (error) {
        console.error("Figure mount failed", error);
        this.replaceChildren();
        return;
      }
      const zone = this.closest("[data-hl-zone]");
      if (zone) {
        const answer = figure.answer ?? ANSWER[figure.name] ?? [200, 160];
        const send = (type: string, at: readonly number[]) => {
          const r = stage.getBoundingClientRect();
          // not bubbling: the zone must not hear its own events
          stage.dispatchEvent(
            new win.PointerEvent(type, {
              pointerType: "mouse",
              pointerId: 1,
              bubbles: false,
              clientX: r.left + (at[0] / 400) * r.width,
              clientY: r.top + (at[1] / 320) * r.height,
            }),
          );
        };
        const move = (event: Event) => {
          const target = event.target as Element | null;
          if (target && stage.contains(target)) return;
          const own = target?.closest?.("[data-hl-at]");
          send(
            "pointermove",
            own
              ? (own.getAttribute("data-hl-at") ?? "").split(",").map(Number)
              : answer,
          );
        };
        const leave = () => send("pointerleave", [0, 0]);
        zone.addEventListener("pointermove", move);
        zone.addEventListener("pointerleave", leave);
        this.off = () => {
          zone.removeEventListener("pointermove", move);
          zone.removeEventListener("pointerleave", leave);
        };
      }
      if (this.getAttribute("scan") === "true") this.handle.scan?.(true);
    }

    private unmount() {
      this.generation++;
      this.off?.();
      this.off = null;
      const handle = this.handle;
      this.handle = null;
      try {
        handle?.destroy();
      } catch (error) {
        console.error("Figure cleanup failed", error);
      }
      this.replaceChildren();
    }
  }
  win.customElements.define("hl-figure", HLFigure);
}
