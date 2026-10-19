/**
 * The AppShell every page sits in: the sidebar, the main column with its top bar, the toaster,
 * and the one menu that may be open, with the scrim that closes it.
 */

import { activeChat } from "../state/project";
import type { App, ThemeChoice } from "../app/app";
import { lang, STRINGS, t, type Lang } from "../i18n";
import { SITE } from "./icons";
import { MARK_BARE } from "../../../../../design-system/marks";
import {
  append,
  button,
  fill,
  h,
  icon,
  iconButton,
  mark,
  tooltip,
  type Children,
} from "./dom";

/** The website, in the language in use. */
export function siteLink(): string {
  return lang === "zh" ? `${SITE}zh/` : SITE;
}

/** The open menu: which one, and on which side of its trigger it opens. */
export class Menus {
  open: string | null = null;
  private left = false;
  constructor(private readonly changed: () => void) {}

  /** Opens `name` under its trigger, towards whichever side of the window has room for `width`. */
  toggle(name: string, event: Event, width: number) {
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    this.left = rect.left + width <= innerWidth - 8 || rect.right - width < 8;
    this.open = this.open === name ? null : name;
    this.changed();
  }

  close() {
    if (this.open === null) return;
    this.open = null;
    this.changed();
  }

  /** The menu's inline placement: layout glue only. */
  side(): Partial<CSSStyleDeclaration> {
    return this.left
      ? { left: "0", right: "auto" }
      : { left: "auto", right: "0" };
  }
}

/** A page in the shell: its main column, rendered again on every change. */
export interface Page {
  readonly el: HTMLElement;
  update(): void;
  destroy?(): void;
  /** Whether the views crowd the window, so the sidebar folds to its rail. */
  crowded?(): boolean;
  /**
   * The page renders all its words in `update`, so a new language keeps it rather than mounting
   * it again: what it holds, such as an open portal window, survives.
   */
  readonly relabels?: boolean;
}

export function resolvedTheme(choice: ThemeChoice): "light" | "dark" {
  if (choice !== "system") return choice;
  return matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

/** Whether the sidebar shows as its rail. The person's own choice wins over the window's width. */
export function collapsed(app: App, page: Page | null): boolean {
  if (app.sidebar) return app.sidebar === "closed";
  return innerWidth < 1180 || !!page?.crowded?.();
}

/** The trigger that folds and opens the sidebar. */
export function sidebarTrigger(app: App, page: Page | null) {
  const folded = collapsed(app, page);
  const label = folded ? t.expandSidebar : t.collapseSidebar;
  return h(
    "button",
    {
      class:
        "bc-button bc-button--ghost bc-button--icon bc-sidebar-trigger le-desk",
      type: "button",
      "aria-label": label,
      "aria-expanded": String(!folded),
      onClick: () => app.setSidebar(folded ? "open" : "closed"),
    },
    icon("panel-left"),
    tooltip(label),
  );
}

/** The light, dark and system theme toggle. */
export function themeToggle(app: App) {
  const choice = (
    value: ThemeChoice,
    name: "sun" | "moon" | "monitor",
    label: string,
  ) =>
    h(
      "button",
      {
        type: "button",
        "aria-pressed": String(app.theme === value),
        "aria-label": label,
        onClick: () => app.setTheme(value),
      },
      icon(name),
    );
  return h(
    "div",
    { class: "bc-segmented", role: "group", "aria-label": t.theme },
    choice("light", "sun", t.light),
    choice("dark", "moon", t.dark),
    choice("system", "monitor", t.system),
  );
}

/** The language menu: each language by its own name, the one in use checked. */
export function languageMenu(app: App, menus: Menus) {
  const open = menus.open === "lang";
  return h(
    "span",
    { class: "bc-menu-anchor" },
    h(
      "button",
      {
        class: "bc-button bc-button--ghost bc-button--sm",
        type: "button",
        "aria-label": `${t.language}: ${t.name}`,
        "aria-haspopup": "menu",
        "aria-expanded": String(open),
        onClick: (event: Event) => menus.toggle("lang", event, 160),
      },
      icon("languages"),
      h("span", { class: "le-wide" }, t.name),
      icon("chevron-down"),
    ),
    open &&
      h(
        "div",
        {
          class: "bc-menu le-menu",
          role: "menu",
          "aria-label": t.language,
          style: { ...menus.side(), minWidth: "160px" },
        },
        (["zh", "en"] as Lang[]).map((id) =>
          h(
            "button",
            {
              class: "bc-menu-item",
              type: "button",
              role: "menuitemradio",
              "aria-checked": String(id === lang),
              lang: STRINGS[id].tag,
              onClick: () => {
                menus.close();
                app.setLang(id);
              },
            },
            h("span", { class: "le-grow" }, STRINGS[id].name),
            id === lang && icon("check"),
          ),
        ),
      ),
  );
}

/** A top bar: the trigger, a divider, then the page's own items. */
export function topbar(app: App, page: Page | null, ...items: Children[]) {
  return h(
    "header",
    { class: "bc-topbar" },
    sidebarTrigger(app, page),
    ...items,
  );
}

/** The phone's way back, where the sidebar would be. */
export function phoneBack(label: string, href: string) {
  return h(
    "a",
    {
      class:
        "bc-button bc-button--ghost bc-button--sm bc-button--back le-phone",
      href,
    },
    icon("arrow-left"),
    label,
  );
}

/** The device portal's address, short: its last number, as the sidebar shows it. */
export function shortAddress(address: string): string {
  const host = address
    .replace(/^https?:\/\//, "")
    .split("/")[0]
    .split(":")[0];
  const last = /\.(\d+)$/.exec(host);
  return last ? `.${last[1]}` : host;
}

function sidebar(app: App, folded: boolean) {
  const route = app.route;
  const project = app.project();
  const navItem = (
    href: string,
    name: Parameters<typeof icon>[0] | null,
    label: string,
    current: boolean,
    extra?: Children,
  ) =>
    h(
      "a",
      {
        class: "bc-nav-item",
        href,
        "aria-current": current ? "page" : "false",
        "aria-label": label,
      },
      name && icon(name),
      h(
        "span",
        { class: "bc-nav-text", style: extra ? { flex: "1 1 auto" } : {} },
        label,
      ),
      extra,
      tooltip(label, "bc-tooltip--right"),
    );
  const apps = app.apps();
  const chats = project && route.page === "app" ? project.chats : [];
  const current = project ? activeChat(project) : null;
  return h(
    "nav",
    {
      class: `bc bc-sidebar le-side${folded ? " bc-sidebar--collapsed" : ""}`,
      "aria-label": "Live Editor",
    },
    h(
      "a",
      {
        class: "bc-brand",
        href: siteLink(),
        "aria-label": t.website,
      },
      mark(MARK_BARE, 24),
      h(
        "span",
        { class: "bc-wordmark bc-nav-text" },
        "Barracuda",
        h("span", { class: "bc-caption bc-muted" }, "Live Editor"),
      ),
    ),
    h(
      "div",
      { class: "le-group" },
      navItem("#/new", "plus", t.newApp, route.page === "new"),
    ),
    apps.length > 0 &&
      h(
        "div",
        { class: "le-group" },
        h("span", { class: "bc-nav-label" }, t.apps),
        apps.map((item) =>
          navItem(
            `#/app/${item.id}`,
            "app-window",
            item.name,
            route.page === "app" && route.id === item.id,
          ),
        ),
      ),
    chats.length > 0 &&
      project &&
      h(
        "div",
        { class: "le-group le-chats" },
        h("span", { class: "bc-nav-label" }, t.chatsOf(project.name)),
        chats.map((chat) =>
          h(
            "a",
            {
              class: "bc-nav-item",
              href: `#/app/${project.id}`,
              "aria-current": chat.id === current?.id ? "page" : "false",
              onClick: (event: Event) => {
                event.preventDefault();
                app.pickChat(chat.id);
              },
            },
            h("span", { class: "bc-nav-text" }, t.stored(chat.title)),
          ),
        ),
        // A chat that has not started yet is the new chat; there is no second empty one.
        (current?.items.length ?? 0) > 0 &&
          h(
            "a",
            {
              class: "bc-nav-item",
              href: `#/app/${project.id}`,
              onClick: (event: Event) => {
                event.preventDefault();
                app.newChat();
              },
            },
            icon("plus"),
            h("span", { class: "bc-nav-text" }, t.newChat),
          ),
      ),
    h(
      "div",
      { class: "le-group le-bottom" },
      project &&
        app.settings.portal &&
        navItem(
          `#/app/${project.id}/install`,
          "cpu",
          t.device,
          route.page === "install",
          h(
            "span",
            { class: "bc-nav-text bc-mono bc-caption bc-muted" },
            shortAddress(app.settings.portal),
          ),
        ),
      navItem("#/settings", "settings", t.settings, route.page === "settings"),
    ),
  );
}

/** The toaster: the one toast showing. */
function toaster(app: App) {
  const toast = app.toast;
  if (!toast) return null;
  return h(
    "div",
    { class: "bc-toaster" },
    h(
      "div",
      {
        class: `bc-toast bc-toast--${toast.tone}`,
        role: toast.tone === "error" ? "alert" : "status",
      },
      icon(
        toast.tone === "error" ? "circle-alert" : "circle-check",
        "bc-toast__icon",
      ),
      h(
        "div",
        { class: "bc-toast__body" },
        h("span", { class: "bc-toast__title" }, toast.title),
        toast.text && h("span", { class: "bc-small bc-muted" }, toast.text),
      ),
      h(
        "button",
        {
          class: "bc-toast__close",
          type: "button",
          "aria-label": t.close,
          onClick: () => app.dismissToast(),
        },
        icon("x"),
      ),
    ),
  );
}

/** Mounts the shell in `root` and renders the page the route names. */
export class Shell {
  readonly menus: Menus;
  private page: Page | null = null;
  private pageKey = "";
  private pageLang = lang;
  private readonly frame = h("div", { class: "bc bc-app le-app" });
  private readonly side = h("div", { style: { display: "contents" } });
  private readonly main = h("div", { class: "bc-main le-main" });
  private readonly overlay = h("div", { style: { display: "contents" } });

  constructor(
    private readonly app: App,
    root: HTMLElement,
    private readonly pages: (shell: Shell) => Page,
  ) {
    this.menus = new Menus(() => this.render());
    append(this.frame, this.side, this.main, this.overlay);
    root.replaceChildren(this.frame);
    app.addEventListener("change", () => this.render());
    addEventListener("resize", () => this.render());
    matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () =>
      this.render(),
    );
    window.addEventListener("keydown", (event) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "b") {
        event.preventDefault();
        app.setSidebar(collapsed(app, this.page) ? "open" : "closed");
      }
      if (event.key === "Escape") this.menus.close();
    });
  }

  private scheduled = false;
  private sideKey = "";
  /** Renders on the next frame, once however many changes came. */
  render() {
    if (this.scheduled) return;
    this.scheduled = true;
    requestAnimationFrame(() => {
      this.scheduled = false;
      this.draw();
    });
  }

  private draw() {
    const app = this.app;
    const key = `${app.route.page}:${app.openId() ?? ""}`;
    // A new language mounts the page again, in its words, unless it relabels itself.
    if (
      key !== this.pageKey ||
      (lang !== this.pageLang && !this.page?.relabels)
    ) {
      this.page?.destroy?.();
      this.menus.open = null;
      this.pageKey = key;
      this.pageLang = lang;
      this.page = this.pages(this);
      fill(this.main, this.page.el);
    }
    const theme = resolvedTheme(app.theme);
    this.frame.setAttribute("data-theme", theme);
    document.documentElement.setAttribute("data-theme", theme);
    const folded = collapsed(app, this.page);
    this.frame.classList.toggle("bc-app--collapsed", folded);
    // The sidebar changes rarely; console lines and frames must not rebuild it under the pointer.
    const project = app.project();
    const sideKey = JSON.stringify([
      lang,
      folded,
      app.route,
      app.apps().map((item) => [item.id, item.name]),
      project?.chats.map((chat) => [chat.id, chat.title]),
      project?.chat,
      app.settings.portal,
    ]);
    if (sideKey !== this.sideKey) {
      this.sideKey = sideKey;
      fill(this.side, sidebar(app, folded));
    }
    this.page?.update();
    fill(
      this.overlay,
      this.menus.open &&
        h("div", {
          class: "le-scrim",
          "aria-hidden": "true",
          onClick: () => this.menus.close(),
        }),
      toaster(app),
    );
  }
}

export { button, iconButton };
