import type { Lang } from "../src/contract";
import { h, icon, pick, twoDigits, type Children, type Text } from "./dom";
import { ICON_CHECK, ICON_EXTERNAL_LINK } from "./icons";
import { qrSvg } from "./qr";

/**
 * The design's form blocks (PageForm's `ok`, `qr`, `linkqr` and `note`): pieces a settings row
 * holds beside or under its fields. Pass them as a row's `blocks`, or place them yourself.
 */

export interface ResultCardOptions {
  /** What was found: a bot's name, 「微信已绑定」. */
  title: Text;
  /** The state badge: 「已验证」, 「已配置」; the signal chip only when {@link ResultCardOptions.live}. */
  badge: Text;
  /** The badge marks a live connection (「已连接」): the green signal chip instead of a neutral badge. */
  live?: boolean;
  /** A machine value under the title, in mono (`@barracuda_home_bot`, a URL). */
  sub?: string;
  /** One or two characters in the 40px tile (a name's initial); a check mark when absent. */
  initial?: string;
  /** Key-value rows under the head; values are mono unless the row's third item is `false` (「已启用」). */
  rows?: readonly (readonly [Text, Children, boolean?])[];
  /** One action under everything, for example an outline `button(…, { size: "sm" })`. */
  action?: Node;
}

/** A result the device or a service reported: a tile, a name, a mono value, a badge, rows and one action. */
export function resultCard(
  options: ResultCardOptions,
  lang: Lang,
): HTMLElement {
  const tile = h(
    "span",
    { class: "bc-option-icon" },
    options.initial ?? icon(ICON_CHECK),
  );
  const text = h(
    "span",
    null,
    h("span", { class: "bc-option-title" }, pick(options.title, lang)),
    options.sub
      ? h("span", { class: "bc-mono bc-small bc-muted" }, options.sub)
      : null,
  );
  text.style.cssText =
    "flex:1 1 auto;min-width:0;display:flex;flex-direction:column;gap:2px;overflow-wrap:anywhere";
  const head = h(
    "div",
    null,
    tile,
    text,
    h(
      "span",
      {
        class: `bc-badge${options.live ? " bc-badge--signal" : ""}`,
        style: { flex: "none", whiteSpace: "nowrap" },
      },
      pick(options.badge, lang),
    ),
  );
  head.style.cssText = "display:flex;align-items:center;gap:12px";
  const rows = options.rows?.length
    ? h(
        "dl",
        { class: "bc-kv" },
        options.rows.map(([label, value, mono = true]) => [
          h("dt", null, pick(label, lang)),
          h("dd", { class: mono ? "bc-mono" : undefined }, value),
        ]),
      )
    : null;
  const body = h(
    "div",
    { class: "bc-card__body" },
    head,
    rows,
    options.action ? h("div", null, options.action) : null,
  );
  body.style.cssText = "display:flex;flex-direction:column;gap:12px";
  return h("div", { class: "bc-card", role: "status" }, body);
}

/**
 * Numbered steps (`.bc-steps`): `radius-sm` marks with mono numbers (`01`). Steps before `done`
 * (`.bc-step--done`) show a check in `success`, step `current` (`aria-current="step"`) fills
 * `muted`, the rest stay muted. `current: -1` marks none.
 */
export function stepList(
  steps: readonly Text[],
  state: { current: number; done: number },
  lang: Lang,
): HTMLOListElement {
  return h(
    "ol",
    { class: "bc-steps" },
    steps.map((step, index) => {
      const done = index < state.done;
      return h(
        "li",
        {
          class: `bc-step${done ? " bc-step--done" : ""}`,
          "aria-current": index === state.current ? "step" : undefined,
        },
        h(
          "span",
          { class: "bc-step__mark", "aria-hidden": "true" },
          done ? icon(ICON_CHECK) : twoDigits(index + 1),
        ),
        h("span", null, pick(step, lang)),
      );
    }),
  );
}

/**
 * A small muted line: `text`, an optional mono value (muted like the line: mono, not emphasis), and
 * an optional link button (`button.bc-link`) after a middle dot (「验证码已发到 you@example.com · 重新发送」).
 */
export function note(
  text: Text,
  lang: Lang,
  options: {
    mono?: string;
    action?: { label: Text; onClick: (event: Event) => void };
  } = {},
): HTMLParagraphElement {
  const value = options.mono
    ? h("span", { class: "bc-mono" }, options.mono)
    : null;
  const action = options.action
    ? [
        h("span", { "aria-hidden": "true" }, "·"),
        h(
          "button",
          { class: "bc-link", type: "button", onclick: options.action.onClick },
          pick(options.action.label, lang),
        ),
      ]
    : null;
  const line = h(
    "p",
    { class: "bc-small bc-muted" },
    pick(text, lang),
    value,
    action,
  );
  line.style.cssText =
    "margin:0;display:flex;flex-wrap:wrap;align-items:center;gap:6px";
  return line;
}

/**
 * A QR Code of `data` on a plate that stays light in the dark theme (`.bc-qr`, `data-theme="light"`:
 * scanners need dark on light). `data: null` draws the empty plate (while a code loads). `dim`
 * fades the code (`.bc-qr--dim`) and writes the label over it (`.bc-qr__overlay`: 「已扫码」,
 * 「二维码已过期」).
 */
export function qrPlate(
  data: string | null,
  px: number,
  lang: Lang,
  options: { dim?: Text } = {},
): HTMLElement {
  const dim = options.dim !== undefined;
  return h(
    "div",
    { class: `bc-qr${dim ? " bc-qr--dim" : ""}`, "data-theme": "light" },
    data === null ? emptyCode(px) : qrSvg(data, px),
    dim
      ? h("div", { class: "bc-qr__overlay" }, pick(options.dim!, lang))
      : null,
  );
}

/** The plate's code-sized space while there is no code yet. */
function emptyCode(px: number): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("width", String(px));
  svg.setAttribute("height", String(px));
  svg.setAttribute("aria-hidden", "true");
  return svg;
}

/** A link a phone opens by scanning: its QR Code, the URL in mono and an outline button that opens it. */
export function qrLink(
  url: string,
  label: Text,
  lang: Lang,
  options: { px?: number } = {},
): HTMLElement {
  const open = h(
    "a",
    {
      class: "bc-button bc-button--outline",
      href: url,
      target: "_blank",
      rel: "noreferrer",
    },
    icon(ICON_EXTERNAL_LINK),
    pick(label, lang),
  );
  const text = h(
    "div",
    null,
    h("span", { class: "bc-mono bc-small" }, url),
    h("div", null, open),
  );
  text.style.cssText =
    "flex:1 1 200px;min-width:0;display:flex;flex-direction:column;gap:12px;overflow-wrap:anywhere";
  const body = h(
    "div",
    { class: "bc-card__body" },
    qrPlate(url, options.px ?? 120, lang),
    text,
  );
  body.style.cssText =
    "display:flex;flex-wrap:wrap;gap:20px;align-items:center";
  return h("div", { class: "bc-card" }, body);
}

/**
 * Copies `text` to the clipboard. The portal is plain HTTP, where `navigator.clipboard` is missing,
 * so this falls back to a selected, off-screen textarea. Resolves whether it worked.
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fall through to the selection copy
  }
  const area = h("textarea", { readonly: true, "aria-hidden": "true" });
  area.value = text;
  area.style.cssText = "position:fixed;top:0;left:-9999px;opacity:0";
  document.body.append(area);
  try {
    area.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    area.remove();
  }
}
