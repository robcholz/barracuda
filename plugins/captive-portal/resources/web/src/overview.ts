import type { EntryStatus, Lang, WebEntry, WebGroup } from "./contract";
import { GROUPS } from "./runtime";
import { STRINGS, joinZh } from "./i18n";
import { assetIcon, h, icon, mark, twoDigits } from "../ui/dom";
import { ICON_ARROW_RIGHT, ICON_CHECK, ICON_CHEVRON_RIGHT } from "../ui/icons";
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

/** Looks up an entry's latest `/portal/status` record. */
export type StatusOf = (id: string) => EntryStatus | null;

export interface OverviewView {
  /** In navigation order (see `sortEntries`). */
  entries: readonly WebEntry[];
  lang: Lang;
  /** The page came over plain HTTP: the connection row and the sidebar footer say so. */
  plain: boolean;
  /** Entry statuses; none when absent. {@link paintStatus} redraws them in place. */
  status?: StatusOf;
}

const NO_STATUS: StatusOf = () => null;

/**
 * What an entry's aside shows: its `detail` in mono (a network name) or, where the aside is a
 * `label` aside (channel rows), its label when it has no detail. `null` leaves the slot empty.
 */
export function asideOf(
  status: EntryStatus | null,
  lang: Lang,
  label = false,
): { text: string; mono: boolean } | null {
  if (status?.detail) return { text: status.detail, mono: true };
  if (label && status?.label) return { text: status.label[lang], mono: false };
  return null;
}

/** The first device entry that reports a status: the top bar badge and the overview's first k/v row. */
export function deviceStatus(
  entries: readonly WebEntry[],
  status: StatusOf,
): { entry: WebEntry; status: EntryStatus } | null {
  for (const entry of entries) {
    if (entry.group !== "device") continue;
    const found = status(entry.id);
    if (found) return { entry, status: found };
  }
  return null;
}

/** 「Wi-Fi 已连接」 / "Wi-Fi connected": an entry's title and its label as one phrase. */
export function titledLabel(entry: WebEntry, label: string, lang: Lang) {
  if (lang === "zh") return joinZh(entry.title.zh, label);
  // sentence case after the title, unless the label starts with an acronym
  const lower = /^[A-Z][a-z]/.test(label)
    ? label[0].toLowerCase() + label.slice(1)
    : label;
  return `${entry.title.en} ${lower}`;
}

/**
 * A group's 「开始使用」 step is done when one of its entries is `ready`. The line names it: the
 * device step with its detail in mono (「已连接 `HomeNet`」), others by title and label. Entries
 * with a label are preferred, so the line names what the entry reports. Returns the words and the
 * machine value, if any.
 */
export function stepDone(
  members: readonly WebEntry[],
  status: StatusOf,
  lang: Lang,
): [string, string?] | null {
  const ready = members
    .map((entry) => ({ entry, status: status(entry.id) }))
    .filter(
      (item): item is { entry: WebEntry; status: EntryStatus } =>
        item.status?.state === "ready",
    );
  const pick = ready.find((item) => item.status.label) ?? ready[0];
  if (!pick) return null;
  const { entry, status: found } = pick;
  if (entry.group === "device" && found.detail)
    return [STRINGS[lang].connectedTo, found.detail];
  return [
    [entry.title[lang], found.label?.[lang], found.detail]
      .filter(Boolean)
      .join(" · "),
  ];
}

/**
 * An empty aside slot; {@link paintStatus} fills it. A label is `.bc-small`; a detail is mono, at
 * the size `extra` gives it, or `.bc-small` with `small` (phone rows).
 */
function slot(
  entry: WebEntry,
  show: "detail" | "label",
  extra = "",
  small = false,
) {
  return h("span", {
    class: `portal-aside portal-aside--${show}${extra ? ` ${extra}` : ""}`,
    "data-status-for": entry.id,
    "data-status-show": show,
    "data-status-small": small,
  });
}

/**
 * Redraws the status parts of an overview (desktop or phone) in place, so live figures keep
 * running: tile and row asides, the device k/v row and the 「开始使用」 steps.
 */
export function paintStatus(root: ParentNode, view: OverviewView) {
  const { lang, entries } = view;
  const status = view.status ?? NO_STATUS;
  for (const node of root.querySelectorAll<HTMLElement>("[data-status-for]")) {
    const found = status(node.getAttribute("data-status-for")!);
    const aside = asideOf(
      found,
      lang,
      node.getAttribute("data-status-show") === "label",
    );
    node.textContent = aside?.text ?? "";
    node.classList.toggle("bc-mono", !!aside?.mono);
    node.classList.toggle(
      "bc-small",
      !!aside && (!aside.mono || node.hasAttribute("data-status-small")),
    );
    node.style.display = aside ? "" : "none";
    if (found) node.setAttribute("data-state", found.state);
    else node.removeAttribute("data-state");
  }
  // the device's two k/v rows: 「状态」 with its label, then its title with its detail (the
  // network name) in mono, or 「—」 when the device does not know it
  const device = deviceStatus(entries, status);
  for (const node of root.querySelectorAll<HTMLElement>(
    "[data-status-device]",
  )) {
    node.style.display = device ? "" : "none";
    if (!device) continue;
    const state = node.getAttribute("data-status-device") === "state";
    const { label, detail } = device.status;
    node.replaceChildren(
      node.tagName === "DT"
        ? state
          ? STRINGS[lang].kvStatus
          : device.entry.title[lang]
        : state
          ? label
            ? h(
                "span",
                {
                  class: `bc-badge${device.status.state === "ready" ? " bc-badge--signal" : ""}`,
                },
                label[lang],
              )
            : "—"
          : (detail ?? "—"),
    );
    node.classList.toggle(
      "bc-mono",
      !state && node.tagName === "DD" && !!detail,
    );
  }
  for (const item of root.querySelectorAll<HTMLElement>("[data-step]")) {
    const group = item.getAttribute("data-step") as WebGroup;
    const done = stepDone(
      entries.filter((entry) => entry.group === group),
      status,
      lang,
    );
    const link = item.querySelector<HTMLElement>(".portal-step__link");
    const line = item.querySelector<HTMLElement>(".portal-step__done");
    if (link) link.style.display = done ? "none" : "";
    if (line) {
      line.style.display = done ? "" : "none";
      line.lastElementChild!.replaceChildren(
        done?.[0] ?? "",
        done?.[1] ? " " : "",
        done?.[1] ? h("span", { class: "bc-mono" }, done[1]) : "",
      );
    }
  }
  const next = root.querySelector<HTMLElement>("[data-next-step]");
  if (next) {
    const steps = groupsOf(entries);
    const index = steps.findIndex(
      ({ entries: members }) => !stepDone(members, status, lang),
    );
    next.hidden = index < 0;
    if (index >= 0) {
      const t = STRINGS[lang];
      const { group, entries: members } = steps[index];
      const row = next.firstElementChild as HTMLAnchorElement;
      row.href = `#${members[0].id}`;
      const [count, text] = row.children;
      count.textContent = `${twoDigits(index + 1)}/${twoDigits(steps.length)}`;
      text.firstElementChild!.textContent = t.next(t.steps[group]);
      text.lastElementChild!.textContent = t.stepHints[group];
    }
  }
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
    { class: "bc-tile__figure portal-tile__figure" },
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
        slot(entry, "detail", "bc-caption bc-muted"),
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
      {
        class: "bc-tile__figure bc-tile__figure--side portal-channels__figure",
      },
      h("hl-figure", { name: "riffle" }),
    ),
    h(
      "div",
      { class: "portal-channels__list" },
      h(
        "div",
        { class: "bc-list-head" },
        h(
          "span",
          { class: "portal-section__head" },
          h("span", { class: "bc-title" }, t.channels),
          h(
            "span",
            { class: "bc-mono bc-caption bc-muted" },
            twoDigits(rows.length),
          ),
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
          h(
            "span",
            { class: "bc-option-title portal-channel-row__name" },
            entry.title[lang],
          ),
          h(
            "span",
            {
              class:
                "bc-small bc-muted portal-grow portal-channel-row__summary",
              title: entry.summary[lang],
            },
            entry.summary[lang],
          ),
          slot(entry, "label", "bc-muted"),
          icon(ICON_CHEVRON_RIGHT),
        ),
      ),
    ),
  );
}

/** The desktop overview, from the manifest only (the design's PageOverview). */
export function renderOverview(view: OverviewView) {
  const { entries, lang, plain } = view;
  const t = STRINGS[lang];
  const layout = layoutModules(entries);
  const steps = groupsOf(entries);
  const stepLink = (group: WebGroup, entry: WebEntry) =>
    group === "channel"
      ? t.tryFirst(entry.title[lang])
      : t.open(entry.title[lang]);
  const root = h(
    "div",
    { class: "bc-page portal-overview" },
    h(
      "section",
      { class: "bc-header bc-header--hero", "aria-labelledby": "hero" },
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
          { class: "bc-kv portal-hero__kv" },
          h("dt", { "data-status-device": "state" }),
          h("dd", { "data-status-device": "state" }),
          h("dt", { "data-status-device": "name" }),
          h("dd", { "data-status-device": "name" }),
          h("dt", null, term(t.kvPages, t.pagesTip)),
          h("dd", { class: "bc-mono" }, String(entries.length)),
          h("dt", null, t.kvConn),
          h(
            "dd",
            null,
            h("span", { class: "bc-mono" }, plain ? "HTTP" : "HTTPS"),
            plain ? ` · ${t.plain}` : null,
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
                { class: "bc-grid__cell", "data-step": group },
                h(
                  "span",
                  { class: "bc-mono bc-caption bc-muted" },
                  twoDigits(index + 1),
                ),
                h("span", { class: "bc-title" }, t.steps[group]),
                h(
                  "span",
                  { class: "bc-status bc-success portal-step__done" },
                  icon(ICON_CHECK),
                  h("span"),
                ),
                h(
                  "a",
                  {
                    class: "bc-link bc-small portal-step__link",
                    href: `#${members[0].id}`,
                  },
                  stepLink(group, members[0]),
                  icon(ICON_ARROW_RIGHT),
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
          { class: "bc-mono bc-caption bc-muted" },
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
  paintStatus(root, view);
  return root;
}

/** The phone overview: the board, then the entries as a full-bleed list grouped by caption (the design's MobileHome). */
export function renderPhoneHome(view: OverviewView) {
  const { entries, lang, plain } = view;
  const t = STRINGS[lang];
  const root = h(
    "div",
    { class: "portal-phone-home" },
    h(
      "section",
      { class: "bc-mobile-header portal-phone-hero" },
      h("hl-figure", { name: "board", "aria-label": t.figAlt }),
      h("h1", { class: "bc-mobile-title" }, t.overview),
      h("p", { class: "bc-lead" }, t.phoneLead),
    ),
    // the first step not done; filled and shown by paintStatus
    h(
      "div",
      { class: "bc-list", "data-next-step": "", hidden: true },
      h(
        "a",
        { class: "bc-list-row" },
        h("span", { class: "bc-mono bc-caption bc-muted" }),
        h(
          "span",
          { class: "portal-grow portal-stack" },
          h("span", { class: "bc-option-title" }),
          h("span", { class: "bc-small bc-muted" }),
        ),
        icon(ICON_CHEVRON_RIGHT),
      ),
    ),
    groupsOf(entries).map(({ group, entries: members }) => [
      h("h2", { class: "bc-list-label" }, t.groups[group]),
      h(
        "div",
        { class: "bc-list" },
        members.map((entry) =>
          h(
            "a",
            { class: "bc-list-row", href: `#${entry.id}` },
            entryIcon(entry, 18),
            h("span", { class: "portal-grow" }, entry.title[lang]),
            slot(
              entry,
              group === "channel" ? "label" : "detail",
              "bc-muted",
              true,
            ),
            icon(ICON_CHEVRON_RIGHT),
          ),
        ),
      ),
    ]),
    plain
      ? h(
          "p",
          { class: "bc-list-foot bc-caption bc-muted" },
          term(t.footer, t.footerTip),
        )
      : null,
  );
  paintStatus(root, view);
  return root;
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
      { class: "bc-frame bc-empty portal-status", "data-status": kind },
      kind === "loading"
        ? h(
            "div",
            { class: "portal-skeleton", "aria-hidden": "true" },
            h("span", {
              class: "bc-skeleton bc-skeleton--title",
              style: { width: "40%" },
            }),
            h("span", { class: "bc-skeleton", style: { width: "90%" } }),
            h("span", { class: "bc-skeleton", style: { width: "70%" } }),
            h("span", {
              class: "bc-skeleton bc-skeleton--block",
              style: { marginTop: "12px" },
            }),
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
