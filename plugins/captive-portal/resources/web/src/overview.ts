import type { Lang, WebEntry, WebGroup } from "./contract";
import { GROUPS } from "./runtime";
import { STRINGS } from "./i18n";
import { assetIcon, h, icon, mark, twoDigits } from "../ui/dom";
import { ICON_ARROW_RIGHT, ICON_CHEVRON_RIGHT } from "../ui/icons";
import { MARK_BARE } from "../ui/marks";

/** The riffle's cards 01 to 08, as viewBox points: a channel row raises its own card while the pointer is on it. */
const CARD_AT = [
  "161,149",
  "176,141",
  "190,134",
  "205,126",
  "220,119",
  "234,111",
  "249,104",
  "264,96",
];

export interface OverviewView {
  /** In navigation order (see `sortEntries`). */
  entries: readonly WebEntry[];
  lang: Lang;
  /** The page came over plain HTTP: the connection row and the sidebar footer say so. */
  plain: boolean;
}

export interface ModuleLayout {
  /** Every device and agent entry, and every channel entry with its own figure. */
  tiles: WebEntry[];
  /** Channel entries without a figure: rows in the channels card beside the `riffle`. */
  rows: WebEntry[];
  /** Columns the channels card spans in the three-column grid. */
  span: number;
}

export function layoutModules(entries: readonly WebEntry[]): ModuleLayout {
  const tiles = entries.filter(
    (entry) => entry.group !== "channel" || entry.figure !== null,
  );
  const rows = entries.filter(
    (entry) => entry.group === "channel" && entry.figure === null,
  );
  // the card takes the rest of the last row when two columns are free, otherwise a full row of its own
  const free = 3 - (tiles.length % 3);
  return { tiles, rows, span: free === 2 ? 2 : 3 };
}

/** An entry's icon at `size`, or an empty box of the same size when the manifest has none. */
export function entryIcon(entry: WebEntry, size = 16): HTMLElement {
  if (entry.icon) return assetIcon(entry.icon, size);
  const box = h("span", { class: "bc-asset-icon", "aria-hidden": "true" });
  box.style.cssText = `width:${size}px;height:${size}px;flex:none`;
  return box;
}

export function groupsOf(entries: readonly WebEntry[]) {
  return GROUPS.map((group) => ({
    group,
    entries: entries.filter((entry) => entry.group === group),
  })).filter((section) => section.entries.length > 0);
}

function term(label: string, tip: string, start = true) {
  return h(
    "span",
    { class: "bc-term", tabindex: "0" },
    label,
    h(
      "span",
      {
        class: `bc-tooltip${start ? " bc-tooltip--start" : ""}`,
        role: "tooltip",
      },
      tip,
    ),
  );
}

function tile(entry: WebEntry, lang: Lang) {
  const figure = h(
    "span",
    { class: "portal-tile__figure" },
    entry.figure ? h("hl-figure", { name: entry.id }) : entryIcon(entry, 40),
  );
  return h(
    "a",
    { class: "bc-tile", href: `#${entry.id}`, "data-hl-zone": "" },
    figure,
    h(
      "span",
      { class: "bc-tile__body" },
      h(
        "span",
        { class: "portal-tile__head" },
        h("span", { class: "bc-title portal-grow" }, entry.title[lang]),
        h("span", { class: "bc-tile__arrow" }, icon(ICON_ARROW_RIGHT)),
      ),
      h("span", { class: "bc-small bc-muted" }, entry.summary[lang]),
    ),
  );
}

function channelCard(rows: WebEntry[], span: number, lang: Lang) {
  const t = STRINGS[lang];
  return h(
    "div",
    {
      class: `portal-channels${span === 3 ? " portal-channels--full" : ""}`,
      "data-hl-zone": "",
    },
    h(
      "span",
      { class: "portal-channels__figure" },
      h("hl-figure", { name: "riffle" }),
    ),
    h(
      "div",
      { class: "portal-channels__list" },
      h(
        "div",
        { class: "portal-channels__head" },
        h("span", { class: "bc-title" }, t.channels),
        h(
          "span",
          { class: "bc-mono bc-muted portal-count" },
          twoDigits(rows.length),
        ),
      ),
      rows.map((entry, index) =>
        h(
          "a",
          {
            class: "bc-list-row portal-channel-row",
            href: `#${entry.id}`,
            "data-hl-at": CARD_AT[Math.min(index, CARD_AT.length - 1)],
          },
          entryIcon(entry),
          h("span", { class: "portal-channel-row__name" }, entry.title[lang]),
          h(
            "span",
            { class: "bc-small bc-muted portal-grow" },
            entry.summary[lang],
          ),
          icon(ICON_CHEVRON_RIGHT),
        ),
      ),
    ),
  );
}

/** The desktop overview, from the manifest only (the design's PageOverview). */
export function renderOverview({ entries, lang, plain }: OverviewView) {
  const t = STRINGS[lang];
  const layout = layoutModules(entries);
  const steps = groupsOf(entries);
  const stepLink = (group: WebGroup, entry: WebEntry) =>
    group === "channel"
      ? t.tryFirst(entry.title[lang])
      : t.open(entry.title[lang]);
  return h(
    "div",
    { class: "bc-page portal-overview" },
    h(
      "section",
      { class: "bc-header", "aria-labelledby": "hero" },
      h(
        "div",
        { class: "bc-header__text portal-hero__text" },
        h(
          "div",
          { class: "portal-hero__title" },
          h("h1", { id: "hero", class: "bc-display" }, "Barracuda"),
          h("p", { class: "bc-lead portal-hero__lead" }, t.lead),
        ),
        h(
          "dl",
          { class: "bc-kv bc-mono portal-hero__kv" },
          h("dt", null, term(t.kvPages, t.pagesTip)),
          h("dd", null, String(entries.length)),
          h("dt", null, t.kvConn),
          h(
            "dd",
            null,
            plain ? ["HTTP · ", term(t.plain, t.plainTip)] : "HTTPS",
          ),
        ),
      ),
      h(
        "div",
        { class: "bc-header__figure portal-hero__figure" },
        h("hl-figure", { name: "board", "aria-label": t.figAlt }),
      ),
    ),
    steps.length
      ? h(
          "section",
          { class: "portal-section", "aria-labelledby": "steps" },
          h("h2", { id: "steps", class: "bc-section-title" }, t.start),
          h(
            "ol",
            { class: "bc-grid portal-steps" },
            steps.map(({ group, entries: members }, index) =>
              h(
                "li",
                null,
                h(
                  "span",
                  { class: "bc-mono bc-muted portal-count" },
                  twoDigits(index + 1),
                ),
                h("span", { class: "bc-title" }, t.steps[group]),
                h(
                  "a",
                  {
                    class: "bc-link bc-small portal-step__link",
                    href: `#${members[0].id}`,
                  },
                  stepLink(group, members[0]),
                  icon(ICON_ARROW_RIGHT, 14),
                ),
              ),
            ),
          ),
        )
      : null,
    h(
      "section",
      { class: "portal-section", "aria-labelledby": "modules" },
      h(
        "div",
        { class: "portal-section__head" },
        h("h2", { id: "modules", class: "bc-section-title" }, t.modules),
        h(
          "span",
          { class: "bc-mono bc-muted portal-count" },
          twoDigits(entries.length),
        ),
      ),
      h(
        "div",
        { class: "bc-grid portal-modules" },
        layout.tiles.map((entry) => tile(entry, lang)),
        layout.rows.length ? channelCard(layout.rows, layout.span, lang) : null,
      ),
    ),
  );
}

/** The phone overview: the board, then the entries as a full-bleed list grouped by caption (the design's MobileHome). */
export function renderPhoneHome({ entries, lang, plain }: OverviewView) {
  const t = STRINGS[lang];
  return h(
    "div",
    { class: "portal-phone-home" },
    h(
      "section",
      { class: "portal-phone-hero" },
      h("hl-figure", { name: "board", "aria-label": t.figAlt }),
      h("h1", { class: "bc-page-title" }, t.overview),
      h("p", { class: "bc-lead" }, t.phoneLead),
    ),
    groupsOf(entries).map(({ group, entries: members }) => [
      h("h2", { class: "bc-list-label" }, t.groups[group]),
      h(
        "div",
        { class: "portal-phone-group" },
        members.map((entry) =>
          h(
            "a",
            { class: "bc-list-row", href: `#${entry.id}` },
            entryIcon(entry, 18),
            h("span", { class: "portal-grow" }, entry.title[lang]),
            icon(ICON_CHEVRON_RIGHT),
          ),
        ),
      ),
    ]),
    plain
      ? h(
          "p",
          { class: "bc-muted portal-phone-footer" },
          term(t.footer, t.footerTip),
        )
      : null,
  );
}

export type StatusKind =
  "empty" | "unavailable" | "loading" | "failed" | "manifest";

export interface StatusView {
  kind: StatusKind;
  lang: Lang;
  /** The entry's title, or its ID when the shell never saw it (unavailable only). */
  name?: string;
  refresh(): void;
  back(): void;
  retry(): void;
}

/** No entries, an entry gone, a module loading or a module that failed (the design's PageStatus). */
export function renderStatus({
  kind,
  lang,
  name = "",
  refresh,
  back,
  retry,
}: StatusView) {
  const t = STRINGS[lang];
  const copy =
    kind === "empty"
      ? { ...t.empty, primary: [t.refresh, refresh] as const }
      : kind === "unavailable"
        ? {
            title: t.unavailable.title(name),
            body: t.unavailable.body(name),
            secondary: [t.refresh, refresh] as const,
            primary: [t.back, back] as const,
          }
        : kind === "failed"
          ? {
              ...t.failed,
              secondary: [t.back, back] as const,
              primary: [t.retry, retry] as const,
            }
          : kind === "manifest"
            ? { ...t.manifestFailed, primary: [t.refresh, refresh] as const }
            : { title: t.loading, body: "" };
  const button = (
    [label, run]: readonly [string, () => void],
    outline: boolean,
  ) =>
    h(
      "button",
      {
        class: `bc-button${outline ? " bc-button--outline" : ""}`,
        type: "button",
        onclick: run,
      },
      label,
    );
  const secondary = "secondary" in copy ? copy.secondary : undefined;
  const primary = "primary" in copy ? copy.primary : undefined;
  return h(
    "div",
    { class: "bc-page" },
    h(
      "section",
      { class: "bc-frame portal-status", "data-status": kind },
      kind === "loading"
        ? h(
            "div",
            { class: "portal-skeleton", "aria-hidden": "true" },
            h("span"),
            h("span"),
            h("span"),
            h("span"),
          )
        : h(
            "span",
            { class: "portal-status__figure" },
            h("hl-figure", { name: "plug" }),
          ),
      h(
        "div",
        { class: "portal-status__text", role: "status", "aria-live": "polite" },
        h("h1", { class: "bc-page-title portal-status__title" }, copy.title),
        copy.body ? h("p", { class: "bc-lead" }, copy.body) : null,
      ),
      secondary || primary
        ? h(
            "div",
            { class: "portal-status__actions" },
            secondary ? button(secondary, true) : null,
            primary ? button(primary, false) : null,
          )
        : null,
    ),
  );
}

/** The bare mark, 24px, as the sidebar and phone header show it. */
export function brandMark() {
  return mark(MARK_BARE, 24);
}
