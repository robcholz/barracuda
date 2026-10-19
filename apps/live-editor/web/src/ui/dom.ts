/** Builds DOM: the few helpers every Live Editor page uses. */

import { ICONS, type IconName } from "./icons";
import { t } from "../i18n";

export type Child = Node | string | number | null | undefined | false;
/** Children may nest arrays, as `map` produces them. */
export type Children = Child | Children[];
export type Props = Record<
  string,
  | string
  | number
  | boolean
  | null
  | undefined
  | ((event: Event) => void)
  | Partial<CSSStyleDeclaration>
>;

/**
 * Builds an element. `class` sets the class list; `on<event>` functions become listeners; `true`
 * sets an empty attribute; `false`, `null` and `undefined` are skipped; other values become
 * attributes. Strings and numbers among the children are inserted as text, never as markup.
 */
export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props?: Props | null,
  ...children: Children[]
): HTMLElementTagNameMap[K];
export function h(
  tag: string,
  props?: Props | null,
  ...children: Children[]
): HTMLElement;
export function h(
  tag: string,
  props?: Props | null,
  ...children: Children[]
): HTMLElement {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props ?? {})) {
    if (value === false || value === null || value === undefined) continue;
    if (typeof value === "function" && key.startsWith("on"))
      node.addEventListener(key.slice(2).toLowerCase(), value);
    else if (key === "class") node.className = String(value);
    else if (key === "style" && typeof value === "object")
      Object.assign(node.style, value);
    else if (key === "value" && "value" in node)
      (node as HTMLInputElement).value = String(value);
    else node.setAttribute(key, value === true ? "" : String(value));
  }
  append(node, ...children);
  return node;
}

/** Appends children as {@link h} does. */
export function append(node: Node, ...children: Children[]) {
  for (const child of (children as Child[]).flat(Infinity as 1) as Child[]) {
    if (child === null || child === undefined || child === false) continue;
    node.appendChild(
      typeof child === "string" || typeof child === "number"
        ? document.createTextNode(String(child))
        : child,
    );
  }
}

/** Replaces a node's children. */
export function fill(node: Element, ...children: Children[]) {
  node.replaceChildren();
  append(node, ...children);
}

/** Parses trusted, build-time SVG markup (an icon or the mark); never pass user data. */
export function svg(markup: string): SVGSVGElement {
  const template = document.createElement("template");
  template.innerHTML = markup;
  return template.content.firstElementChild as SVGSVGElement;
}

/** A Lucide icon as `.bc-icon`; `className` adds classes, such as `bc-muted` or `bc-spinner`. */
export function icon(name: IconName, className = ""): SVGSVGElement {
  return svg(
    `<svg class="bc-icon${className ? ` ${className}` : ""}" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name]}</svg>`,
  );
}

/** A tooltip for an icon-only control: the control's own label, shown on hover and focus. */
export function tooltip(text: string, className = "") {
  return h(
    "span",
    {
      class: `bc-tooltip${className ? ` ${className}` : ""}`,
      "aria-hidden": "true",
    },
    text,
  );
}

/** An icon-only button with its label as tooltip (`.bc-icon-button`, or a ghost icon button). */
export function iconButton(
  name: IconName,
  label: string,
  onClick: (event: Event) => void,
  options: {
    ghost?: boolean;
    pressed?: boolean;
    expanded?: boolean;
    disabled?: boolean;
    class?: string;
  } = {},
) {
  return h(
    "button",
    {
      class: `${options.ghost ? "bc-button bc-button--ghost bc-button--icon" : "bc-icon-button"}${options.class ? ` ${options.class}` : ""}`,
      type: "button",
      "aria-label": label,
      "aria-pressed":
        options.pressed === undefined ? null : String(options.pressed),
      "aria-expanded":
        options.expanded === undefined ? null : String(options.expanded),
      disabled: !!options.disabled,
      onClick,
    },
    icon(name),
    tooltip(label),
  );
}

/** A `.bc-button`: `kind` picks outline, ghost or danger; `sm` the small size. */
export function button(
  label: Children,
  onClick: (event: Event) => void,
  options: {
    kind?: "outline" | "ghost" | "danger";
    sm?: boolean;
    icon?: IconName;
    disabled?: boolean;
    type?: string;
    class?: string;
  } = {},
) {
  const classes = ["bc-button"];
  if (options.kind) classes.push(`bc-button--${options.kind}`);
  if (options.sm) classes.push("bc-button--sm");
  if (options.class) classes.push(options.class);
  return h(
    "button",
    {
      class: classes.join(" "),
      type: options.type ?? "button",
      disabled: !!options.disabled,
      onClick,
    },
    options.icon && icon(options.icon),
    label,
  );
}

/** A status line: an icon and its words, coloured by `tone`. */
export function status(
  tone: "success" | "destructive" | "muted" | "warning",
  name: IconName | null,
  ...text: Children[]
) {
  return h(
    "span",
    { class: `bc-status bc-${tone}`, role: "status" },
    name && icon(name, name === "loader-circle" ? "bc-spinner" : ""),
    ...text,
  );
}

/** Formats a byte count as the design system writes sizes. */
export function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** How long ago `at` was, in the words a list shows. */
export function ago(at: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - at) / 1000));
  if (s < 45) return t.justNow;
  const m = Math.round(s / 60);
  if (m < 60) return t.minAgo(m);
  const hours = Math.round(m / 60);
  if (hours < 24) return t.hoursAgo(hours);
  return new Date(at).toLocaleDateString(t.tag);
}
