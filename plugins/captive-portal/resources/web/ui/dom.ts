import type { Lang, PortalText } from "../src/contract";

/** A string in both portal languages, or one string used for both (product names, machine values). */
export type Text = string | PortalText;

/** Picks the string for `lang`. */
export function pick(text: Text, lang: Lang): string {
  return typeof text === "string" ? text : text[lang];
}

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
 * Builds an element. `class` sets the class list; `on<event>` functions become listeners;
 * `true` sets an empty attribute; `false`, `null` and `undefined` are skipped; other values become
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

/** Parses trusted, build-time SVG markup (an icon or mark constant); never pass user data. */
function svgFrom(markup: string): SVGSVGElement {
  const template = document.createElement("template");
  template.innerHTML = markup;
  return template.content.firstElementChild as SVGSVGElement;
}

/**
 * A Lucide icon (`ICON_*` from `./icons`) as `.bc-icon`, 16px unless `size` says otherwise. `className`
 * adds classes, such as a text colour (`bc-muted`, `bc-warning`) or a component part (`bc-toast__icon`).
 */
export function icon(
  paths: string,
  size?: number,
  className?: string,
): SVGSVGElement {
  const svg = svgFrom(
    `<svg class="bc-icon" viewBox="0 0 24 24" aria-hidden="true">${paths}</svg>`,
  );
  if (className) svg.classList.add(...className.split(" "));
  if (size) svg.style.cssText = `width:${size}px;height:${size}px`;
  return svg;
}

/** A mark (`MARK_*` from `./marks`): brand marks fill `currentColor`, 16px unless `size` says otherwise. */
export function mark(markup: string, size = 16): SVGSVGElement {
  const svg = svgFrom(markup);
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.style.width = `${size}px`;
  svg.style.height = `${size}px`;
  svg.style.flex = "none";
  return svg;
}

const loaded = new Map<string, Promise<string>>();

/** Fetches an asset once (through the window's `fetch`, which the shell queues) as an object URL. */
function objectUrl(url: string): Promise<string> {
  let pending = loaded.get(url);
  if (!pending) {
    pending = fetch(url).then(async (response) => {
      if (!response.ok) throw new Error(`${url}: ${response.status}`);
      return URL.createObjectURL(await response.blob());
    });
    pending.catch(() => loaded.delete(url));
    loaded.set(url, pending);
  }
  return pending;
}

/**
 * An icon from a URL: an `.svg` is drawn as a monochrome mask in `currentColor` (so a brand mark
 * follows the theme), anything else as an image. The file is fetched rather than referenced, so
 * it waits its turn for one of the device's few connections; until it arrives, or if it is
 * missing, the icon is an empty box of the same size.
 */
export function assetIcon(url: string, size = 16): HTMLElement {
  const box = `width:${size}px;height:${size}px;flex:none`;
  const svg = /\.svg(?:$|[?#])/i.test(url);
  const element = svg
    ? h("span", { class: "bc-asset-icon", "aria-hidden": "true" })
    : h("img", { alt: "", width: size, height: size });
  element.style.cssText = svg
    ? `${box};display:inline-block;background:currentColor`
    : `${box};display:block;object-fit:contain`;
  objectUrl(url).then(
    (source) => {
      if (!svg) (element as HTMLImageElement).src = source;
      else {
        const mask = `url("${source}") center/contain no-repeat`;
        element.style.setProperty("-webkit-mask", mask);
        element.style.setProperty("mask", mask);
      }
    },
    () => {},
  );
  return element;
}

/** Two digits, as the design numbers counts and steps (`09`, `01`). */
export function twoDigits(value: number): string {
  return String(value).padStart(2, "0");
}
