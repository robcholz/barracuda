/**
 * Builds DOM for pages drawn with the design system, without a framework: Live Editor and the
 * device portal's shell, UI kit and pages build their elements with these.
 */

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
 * sets an empty attribute; `false`, `null` and `undefined` are skipped; `value` sets a form
 * control's value; other values become attributes. Strings and numbers among the children are
 * inserted as text, never as markup.
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

/** Parses trusted, build-time SVG markup (an icon or a mark); never pass user data. */
export function svg(markup: string): SVGSVGElement {
  const template = document.createElement("template");
  template.innerHTML = markup;
  return template.content.firstElementChild as SVGSVGElement;
}

/**
 * An icon (`ICON_*` from `./icons`) as `.bc-icon`, 16px as the design system draws it. `className`
 * adds classes, such as a text colour (`bc-muted`) or motion (`bc-spinner`).
 */
export function icon(paths: string, className = ""): SVGSVGElement {
  return svg(
    `<svg class="bc-icon${className ? ` ${className}` : ""}" viewBox="0 0 24 24" aria-hidden="true">${paths}</svg>`,
  );
}

/** A mark (`MARK_*` from `./marks`) at `size` px square. */
export function mark(markup: string, size = 16): SVGSVGElement {
  const node = svg(markup);
  node.setAttribute("width", String(size));
  node.setAttribute("height", String(size));
  node.style.width = `${size}px`;
  node.style.height = `${size}px`;
  node.style.flex = "none";
  return node;
}
