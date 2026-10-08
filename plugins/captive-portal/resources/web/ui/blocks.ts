import type { Lang } from "../src/contract";
import { h, icon, pick, type Children, type Text } from "./dom";
import { ICON_CHECK, ICON_EXTERNAL_LINK } from "./icons";
import { qrSvg } from "./qr";

/**
 * The design's form blocks (PageForm's `ok`, `qr`, `linkqr` and `note`): pieces a settings row
 * holds beside or under its fields. Pass them as a row's `blocks`, or place them yourself.
 */

export interface ResultCardOptions {
  /** What was found: a bot's name, 「微信已绑定」. */
  title: Text;
  /** The solid green state chip: 「已验证」, 「已连接」. */
  badge: Text;
  /** A machine value under the title, in mono (`@barracuda_home_bot`, a URL). */
  sub?: string;
  /** One or two characters in the 40px tile (a name's initial); a check mark when absent. */
  initial?: string;
  /** Key-value rows under the head; values are mono. */
  rows?: readonly (readonly [Text, Children])[];
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
  tile.style.fontWeight = "600";
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
        class: "bc-badge bc-badge--signal",
        style: { flex: "none", whiteSpace: "nowrap" },
      },
      pick(options.badge, lang),
    ),
  );
  head.style.cssText = "display:flex;align-items:center;gap:12px";
  let rows: HTMLElement | null = null;
  if (options.rows?.length) {
    rows = h(
      "div",
      null,
      options.rows.map(([label, value]) => {
        const line = h(
          "div",
          null,
          h("span", { class: "bc-muted" }, pick(label, lang)),
          h("span", { class: "bc-mono" }, value),
        );
        line.style.cssText =
          "display:grid;grid-template-columns:120px minmax(0,1fr);padding:8px 0;border-bottom:1px solid var(--border)";
        return line;
      }),
    );
    rows.style.cssText = "border-top:1px solid var(--border);font-size:13px";
  }
  const card = h(
    "div",
    { class: "bc-frame", role: "status" },
    head,
    rows,
    options.action ? h("div", null, options.action) : null,
  );
  card.style.cssText =
    "padding:14px 16px;display:flex;flex-direction:column;gap:12px";
  return card;
}

/**
 * Numbered steps (`ol`): steps before `done` show a check on the signal fill, step `current`
 * (`aria-current="step"`) is outlined in ink, the rest are muted. `current: -1` marks none.
 */
export function stepList(
  steps: readonly Text[],
  state: { current: number; done: number },
  lang: Lang,
): HTMLOListElement {
  const list = h(
    "ol",
    null,
    steps.map((step, index) => {
      const done = index < state.done;
      const current = index === state.current;
      const mark = h(
        "span",
        { "aria-hidden": "true" },
        done ? icon(ICON_CHECK, 14) : String(index + 1),
      );
      mark.style.cssText = `flex:none;display:inline-flex;align-items:center;justify-content:center;width:22px;height:22px;border-radius:11px;font-size:12px;font-weight:500;${
        done
          ? "background:var(--signal);color:var(--signal-foreground)"
          : current
            ? "border:1px solid var(--foreground);color:var(--foreground)"
            : "border:1px solid var(--border);color:var(--muted-foreground)"
      }`;
      const label = h("span", null, pick(step, lang));
      label.style.cssText = current
        ? "font-weight:500"
        : "color:var(--muted-foreground)";
      const item = h(
        "li",
        { "aria-current": current ? "step" : undefined },
        mark,
        label,
      );
      item.style.cssText = "display:flex;align-items:center;gap:10px";
      return item;
    }),
  );
  list.style.cssText =
    "list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:12px";
  return list;
}

/**
 * A small muted line: `text`, an optional mono value in ink, and an optional link-styled button after
 * a middle dot (「验证码已发到 you@example.com · 重新发送」).
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
  if (value) value.style.color = "var(--foreground)";
  let action: HTMLElement[] = [];
  if (options.action) {
    const link = h(
      "button",
      {
        class: "bc-link bc-small",
        type: "button",
        onclick: options.action.onClick,
      },
      pick(options.action.label, lang),
    );
    link.style.cssText = "padding:0;border:0;background:none;cursor:pointer";
    action = [h("span", { "aria-hidden": "true" }, "·"), link];
  }
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
 * A QR Code of `data` on a plate that stays light in the dark theme (scanners need dark on light).
 * `data: null` draws the empty plate (while a code loads). `dim` fades the code and writes the
 * label over it (「已扫码」, 「二维码已过期」).
 */
export function qrPlate(
  data: string | null,
  px: number,
  lang: Lang,
  options: { dim?: Text; padding?: number } = {},
): HTMLElement {
  const code = data === null ? h("div") : h("div", null, qrSvg(data, px));
  code.style.cssText = `width:${px}px;height:${px}px;opacity:${options.dim === undefined ? 1 : 0.08}`;
  const plate = h(
    "div",
    { "data-theme": "light" },
    code,
    options.dim === undefined ? null : h("div", null, pick(options.dim, lang)),
  );
  plate.style.cssText = `position:relative;flex:none;padding:${options.padding ?? 8}px;border:1px solid var(--border);border-radius:var(--radius-md);background:var(--background);color:var(--foreground)`;
  if (options.dim !== undefined)
    (plate.lastElementChild as HTMLElement).style.cssText =
      "position:absolute;inset:0;display:flex;align-items:center;justify-content:center;padding:8px;text-align:center;font-size:13px;font-weight:500";
  return plate;
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
  const card = h(
    "div",
    { class: "bc-frame" },
    qrPlate(url, options.px ?? 120, lang, { padding: 6 }),
    text,
  );
  card.style.cssText =
    "display:flex;flex-wrap:wrap;gap:20px;align-items:center;padding:16px";
  return card;
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
