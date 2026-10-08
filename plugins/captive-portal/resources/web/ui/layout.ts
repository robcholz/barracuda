import type { Lang, PortalContext, PortalModule } from "../src/contract";
import { assetIcon, h, icon, type Children, type Text, pick } from "./dom";

/**
 * A module page from a render function: renders once into the root the shell gives it and empties
 * that root on cleanup. Pass `context.signal` to anything long-lived the page starts.
 */
export function definePage(
  render: (context: PortalContext) => Node,
): PortalModule["mount"] {
  return (root, context) => {
    if (context.signal.aborted) return;
    root.replaceChildren(render(context));
    return () => root.replaceChildren();
  };
}

/** The page column (`.bc-page`): content padded `space-8`, capped at `content-max`. */
export function page(...children: Children[]): HTMLElement {
  return h("div", { class: "bc-page" }, ...children);
}

/** A hairline surface (`.bc-frame`). */
export function frame(...children: Children[]): HTMLElement {
  return h("section", { class: "bc-frame" }, ...children);
}

export interface HeaderOptions {
  title: Text;
  /** One sentence saying what the page is for. */
  lead: Text;
  /** A 28px mark before the title: a URL (`.svg` drawn in `currentColor`, else an image) or a node. */
  icon?: string | Node;
  /** Anything under the lead, for example a {@link kv} table. */
  extra?: Children;
  /** `<hl-figure name>`: a built-in (`riffle`) or an entry ID whose manifest `figure` the shell loads. */
  figure?: string;
  figureLabel?: Text;
  /** The figure's maximum width in px (the design uses 280 to 300). */
  figureWidth?: number;
  /** Starts the figure's ambient sweep, for figures that have one (router). */
  scan?: boolean;
}

/** The page header frame: title, lead and optional extras on the left, the page's figure on the right. */
export function header(options: HeaderOptions, lang: Lang): HTMLElement {
  const mark =
    options.icon === undefined
      ? null
      : typeof options.icon === "string"
        ? assetIcon(options.icon, 28)
        : options.icon;
  const title = h(
    "h1",
    { class: "bc-page-title" },
    mark,
    pick(options.title, lang),
  );
  if (mark) title.style.cssText = "display:flex;align-items:center;gap:12px";
  const lead = h("p", { class: "bc-lead" }, pick(options.lead, lang));
  let text: HTMLElement;
  if (options.extra) {
    const head = h("div", null, title, lead);
    head.style.cssText = "display:flex;flex-direction:column;gap:8px";
    text = h("div", { class: "bc-header__text" }, head, options.extra);
    text.style.cssText = "justify-content:flex-start;gap:20px";
  } else text = h("div", { class: "bc-header__text" }, title, lead);
  const figure = options.figure
    ? h("hl-figure", {
        name: options.figure,
        "aria-label": options.figureLabel
          ? pick(options.figureLabel, lang)
          : undefined,
        scan: options.scan ? "true" : undefined,
      })
    : null;
  if (figure) figure.style.maxWidth = `${options.figureWidth ?? 300}px`;
  return h(
    "section",
    { class: "bc-header" },
    text,
    figure ? h("div", { class: "bc-header__figure" }, figure) : null,
  );
}

/** A key-value table (`.bc-kv`). `live` announces changes politely (status read-outs). */
export function kv(
  rows: readonly (readonly [Text, Children])[],
  lang: Lang,
  options: { live?: boolean; mono?: boolean; labelWidth?: number } = {},
): HTMLDListElement {
  const list = h(
    "dl",
    {
      class: `bc-kv${options.mono ? " bc-mono" : ""}`,
      role: options.live ? "status" : undefined,
      "aria-live": options.live ? "polite" : undefined,
    },
    rows.map(([label, value]) => [
      h("dt", null, pick(label, lang)),
      h("dd", null, value),
    ]),
  );
  list.style.gridTemplateColumns = `${options.labelWidth ?? 112}px minmax(0, 1fr)`;
  return list;
}

/** A fact the reader may want but does not act on: a dotted underline with a one-line tooltip. */
export function term(
  label: Text,
  tip: Text,
  lang: Lang,
  options: { start?: boolean } = {},
): HTMLElement {
  return h(
    "span",
    { class: "bc-term", tabindex: "0" },
    pick(label, lang),
    h(
      "span",
      {
        class: `bc-tooltip${options.start ? " bc-tooltip--start" : ""}`,
        role: "tooltip",
      },
      pick(tip, lang),
    ),
  );
}

/** A badge; `signal` is the solid green live-state chip (「已连接」). */
export function badge(
  label: Text,
  lang: Lang,
  options: { signal?: boolean } = {},
) {
  return h(
    "span",
    { class: `bc-badge${options.signal ? " bc-badge--signal" : ""}` },
    pick(label, lang),
  );
}

export interface ButtonOptions {
  variant?: "primary" | "outline" | "ghost" | "danger";
  size?: "md" | "sm";
  /** An `ICON_*` constant drawn before the label. */
  icon?: string;
  type?: "button" | "submit" | "reset";
  onClick?: (event: Event) => void;
}

/** A button whose label names its result (「保存并替换通道」, never 「提交」). */
export function button(label: Text, lang: Lang, options: ButtonOptions = {}) {
  const variant = options.variant ?? "primary";
  return h(
    "button",
    {
      class: `bc-button${variant === "primary" ? "" : ` bc-button--${variant}`}${options.size === "sm" ? " bc-button--sm" : ""}`,
      type: options.type ?? "button",
      onclick: options.onClick,
    },
    options.icon ? icon(options.icon) : null,
    pick(label, lang),
  );
}

/** A label-left settings row (`.bc-row`): title and one-line hint beside the controls. */
export function row(
  title: Text,
  hint: Text | undefined,
  lang: Lang,
  ...children: Children[]
): HTMLElement {
  return h(
    "div",
    { class: "bc-row" },
    h(
      "div",
      { class: "bc-row__label" },
      h("span", { class: "bc-title" }, pick(title, lang)),
      hint === undefined
        ? null
        : h("span", { class: "bc-small bc-muted" }, pick(hint, lang)),
    ),
    h("div", { class: "bc-row__body" }, ...children),
  );
}
