/** The package's DOM helpers: building elements, and patching one tree into another in place. */

type Child = Node | string | null | undefined | false;
export type Children = Child | Children[];

const ELEMENT_NODE = 1;
const TEXT_NODE = 3;

/**
 * Builds an element. `true` sets an empty attribute; `false`, `null` and `undefined` are skipped; a
 * `style` object is assigned property by property. String children are text, never markup.
 */
export function h(
  tag: string,
  props?: Record<
    string,
    string | number | boolean | null | undefined | Partial<CSSStyleDeclaration>
  > | null,
  ...children: Children[]
): HTMLElement {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props ?? {})) {
    if (value === false || value === null || value === undefined) continue;
    if (key === "style" && typeof value === "object")
      Object.assign(node.style, value);
    else node.setAttribute(key, value === true ? "" : String(value));
  }
  for (const child of (children as Child[]).flat(Infinity as 1) as Child[])
    if (child !== null && child !== undefined && child !== false)
      node.append(child);
  return node;
}

/**
 * Makes `node` what `next` is, keeping `node`'s elements and text nodes wherever `next` has the same
 * kind in the same place; returns the node to use (`next` when the kinds differ). An element marked
 * `data-md-keep` (a code block's actions) is left as it is, and `keep` (the page's caret) is left
 * where it is.
 */
export function morph(node: Node, next: Node, keep?: Node): Node {
  if (node.nodeName !== next.nodeName) return next;
  if (node.nodeType === TEXT_NODE) {
    const text = node as Text;
    const { data } = next as Text;
    // appending keeps a selection inside the text
    if (data.startsWith(text.data))
      text.appendData(data.slice(text.data.length));
    else text.data = data;
    return node;
  }
  const element = node as Element;
  if (node.nodeType !== ELEMENT_NODE || element.hasAttribute("data-md-keep"))
    return node;
  const model = next as Element;
  for (const { name } of [...element.attributes])
    if (!model.hasAttribute(name)) element.removeAttribute(name);
  for (const { name, value } of [...model.attributes])
    if (element.getAttribute(name) !== value) element.setAttribute(name, value);
  const children = [...node.childNodes].filter((child) => child !== keep);
  sync(
    node,
    [...model.childNodes].map((child, i) =>
      children[i] ? morph(children[i], child, keep) : child,
    ),
    keep,
  );
  return node;
}

/** Puts `children` in `parent` in order, moving only what is out of place; new ones go before `keep`. */
export function sync(parent: Node, children: Node[], keep?: Node) {
  let at = parent.firstChild;
  for (const child of children) {
    if (at === child) at = at.nextSibling;
    else parent.insertBefore(child, at);
  }
  while (at) {
    const next = at.nextSibling;
    if (at !== keep) parent.removeChild(at);
    at = next;
  }
}
