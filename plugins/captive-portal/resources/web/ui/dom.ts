import type { Lang, PortalText } from "../src/contract";
import { h, icon as drawIcon } from "../../../../../design-system/dom";

/** A string in both portal languages, or one string used for both (product names, machine values). */
export type Text = string | PortalText;

/** Picks the string for `lang`. */
export function pick(text: Text, lang: Lang): string {
  return typeof text === "string" ? text : text[lang];
}

export {
  append,
  h,
  mark,
  type Child,
  type Children,
  type Props,
} from "../../../../../design-system/dom";

/**
 * A Lucide icon (`ICON_*` from `./icons`) as `.bc-icon`, 16px unless `size` says otherwise. `className`
 * adds classes, such as a text colour (`bc-muted`, `bc-warning`) or a component part (`bc-toast__icon`).
 */
export function icon(
  paths: string,
  size?: number,
  className?: string,
): SVGSVGElement {
  const node = drawIcon(paths, className);
  if (size) node.style.cssText = `width:${size}px;height:${size}px`;
  return node;
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
