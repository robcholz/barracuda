import type { Lang, Toast, WebEntry } from "./contract";
import { ModuleSession, parseEntries, sortEntries } from "./runtime";
import { STRINGS } from "./i18n";
import {
  brandMark,
  entryIcon,
  groupsOf,
  renderOverview,
  renderPhoneHome,
  renderStatus,
  type StatusKind,
} from "./overview";
import { Toaster } from "./toast";
import { defineFigureElement, setFigureSource } from "./live/host";
import { append, h, icon } from "../ui/dom";
import {
  ICON_ARROW_LEFT,
  ICON_CHECK,
  ICON_CHEVRON_DOWN,
  ICON_LANGUAGES,
  ICON_MONITOR,
  ICON_MOON,
  ICON_OVERVIEW,
  ICON_PANEL,
  ICON_SUN,
  ICON_WIFI_OFF,
} from "../ui/icons";

export type ThemeMode = "light" | "dark" | "system";

export const STORAGE = {
  lang: "barracuda.lang",
  theme: "barracuda.theme",
  sidebar: "barracuda.sidebar",
} as const;

/** Below this width the portal is the phone layout: no sidebar, the overview as a list. */
export const PHONE_QUERY = "(max-width: 719px)";
const MANIFEST_URL = "/portal/entries.json";
const POLL_MS = 30_000;
const MANIFEST_TIMEOUT_MS = 8_000;

export interface PortalOptions {
  window?: Window & typeof globalThis;
  /** Mounts the shell here; defaults to `#portal`, then `<body>`. */
  root?: HTMLElement;
  fetch?: (input: string, init?: RequestInit) => Promise<Response>;
  /** Imports a module URL; tests replace it. */
  load?: (url: string) => Promise<unknown>;
  /** How long a success or info toast stays (default 4 s); tests shorten it. */
  toastMs?: number;
}

function stored(win: Window, key: string): string | null {
  try {
    return win.localStorage.getItem(key);
  } catch {
    return null;
  }
}
function store(win: Window, key: string, value: string) {
  try {
    win.localStorage.setItem(key, value);
  } catch {
    // storage may be blocked; the choice still applies to this page
  }
}

/** Reads `#overview` or `#<entry id>`; anything else is a (possibly unknown) entry ID. */
export function routeOf(hash: string): string {
  const id = decodeURIComponent(hash.replace(/^#\/?/, ""));
  return id === "" || id === "overview" ? "overview" : id;
}

/** The running portal shell: one per page. */
export class Portal {
  readonly win: Window & typeof globalThis;
  private readonly doc: Document;
  private readonly fetcher: (
    input: string,
    init?: RequestInit,
  ) => Promise<Response>;
  private readonly load?: (url: string) => Promise<unknown>;
  private readonly session = new ModuleSession();
  readonly toaster: Toaster;

  lang: Lang;
  mode: ThemeMode;
  collapsed: boolean;
  entries: WebEntry[] = [];
  /** Every entry seen this session, so a vanished one can still be named. */
  private readonly known = new Map<string, WebEntry>();
  manifest: "loading" | "ready" | "error" = "loading";
  route = "overview";
  private phone: boolean;
  private langOpen = false;
  private mountedKey: string | null = null;
  private navigation = 0;
  private refreshing: Promise<void> | null = null;
  private request?: AbortController;
  private poll?: ReturnType<typeof setInterval>;
  private readonly media: MediaQueryList;
  private readonly scheme: MediaQueryList;
  private readonly lifecycle = new AbortController();

  readonly app: HTMLElement;
  private readonly sidebar: HTMLElement;
  private readonly desktopBar: HTMLElement;
  private readonly phoneBar: HTMLElement;
  readonly content: HTMLElement;

  constructor(options: PortalOptions = {}) {
    this.win = options.window ?? window;
    this.doc = this.win.document;
    this.fetcher =
      options.fetch ?? ((input, init) => this.win.fetch(input, init));
    this.load = options.load;
    this.lang = stored(this.win, STORAGE.lang) === "en" ? "en" : "zh";
    const mode = stored(this.win, STORAGE.theme);
    this.mode = mode === "light" || mode === "dark" ? mode : "system";
    this.collapsed = stored(this.win, STORAGE.sidebar) === "collapsed";
    this.media = this.win.matchMedia(PHONE_QUERY);
    this.scheme = this.win.matchMedia("(prefers-color-scheme: dark)");
    this.phone = this.media.matches;
    this.toaster = new Toaster(() => STRINGS[this.lang].close, options.toastMs);

    defineFigureElement(this.win);
    setFigureSource(
      (name) => this.entries.find((entry) => entry.id === name)?.figure ?? null,
      options.load,
    );

    this.sidebar = h("nav", { class: "bc-sidebar" });
    this.desktopBar = h("header", {
      class: "bc-topbar portal-topbar portal-topbar--desktop",
    });
    this.phoneBar = h("header", {
      class: "bc-topbar portal-topbar portal-topbar--phone",
    });
    this.content = h("main", {
      id: "content",
      class: "portal-content",
      tabindex: "-1",
    });
    this.app = h(
      "div",
      { class: "bc bc-app" },
      this.sidebar,
      h(
        "div",
        { class: "bc-main" },
        this.desktopBar,
        this.phoneBar,
        this.content,
      ),
      this.toaster.element,
    );
    const host =
      options.root ?? this.doc.getElementById("portal") ?? this.doc.body;
    host.replaceChildren(this.app);
    this.listen();
    this.applyTheme();
    this.applyCollapsed();
    this.route = routeOf(this.win.location.hash);
    this.render(true);
    this.startPolling();
    void this.refresh();
  }

  private get t() {
    return STRINGS[this.lang];
  }

  private get plain() {
    return this.win.location.protocol === "http:";
  }

  private listen() {
    const { signal } = this.lifecycle;
    const win = this.win;
    win.addEventListener("hashchange", () => this.navigate(), { signal });
    this.media.addEventListener(
      "change",
      () => {
        this.phone = this.media.matches;
        if (this.route === "overview") this.renderContent();
      },
      { signal },
    );
    this.scheme.addEventListener("change", () => this.applyTheme(), { signal });
    this.doc.addEventListener(
      "visibilitychange",
      () => {
        if (!this.doc.hidden) void this.refresh();
      },
      { signal },
    );
    this.doc.addEventListener(
      "keydown",
      (event) => {
        if (
          (event.ctrlKey || event.metaKey) &&
          !event.altKey &&
          event.key.toLowerCase() === "b"
        ) {
          event.preventDefault();
          this.toggleSidebar();
        } else if (event.key === "Escape" && this.langOpen) {
          this.langOpen = false;
          this.renderTopbar();
        }
      },
      { signal },
    );
    this.doc.addEventListener(
      "click",
      (event) => {
        if (
          this.langOpen &&
          !(event.target as Element | null)?.closest?.(".bc-menu-anchor")
        ) {
          this.langOpen = false;
          this.renderTopbar();
        }
      },
      { signal },
    );
    win.addEventListener(
      "pagehide",
      () => {
        clearInterval(this.poll);
        this.request?.abort();
        this.leaveModule();
      },
      { signal },
    );
    win.addEventListener(
      "pageshow",
      (event) => {
        if (!(event as PageTransitionEvent).persisted) return;
        this.startPolling();
        this.renderContent();
        void this.refresh();
      },
      { signal },
    );
  }

  /** Stops the shell (tests, and nothing else: a page lives as long as the shell). */
  destroy() {
    this.lifecycle.abort();
    clearInterval(this.poll);
    this.request?.abort();
    this.leaveModule();
    this.toaster.clear();
    this.app.remove();
  }

  // ------------------------------------------------------------ actions

  go(id: string) {
    const hash = `#${id === "overview" ? "overview" : id}`;
    if (this.win.location.hash === hash) this.navigate();
    else this.win.location.hash = hash;
  }

  toast(toast: Toast) {
    this.toaster.show(toast);
  }

  setLang(lang: Lang) {
    this.langOpen = false;
    if (lang === this.lang) return this.renderTopbar();
    this.lang = lang;
    store(this.win, STORAGE.lang, lang);
    // a language change remounts the module: it renders its own strings once, in its context's language
    this.render(true);
  }

  setMode(mode: ThemeMode) {
    this.mode = mode;
    store(this.win, STORAGE.theme, mode);
    this.applyTheme();
    this.renderTopbar();
  }

  toggleSidebar() {
    this.collapsed = !this.collapsed;
    store(this.win, STORAGE.sidebar, this.collapsed ? "collapsed" : "expanded");
    this.applyCollapsed();
    this.renderTopbar();
  }

  /** Fetches the manifest; overlapping calls share one request. */
  refresh(): Promise<void> {
    this.refreshing ??= this.fetchManifest().finally(() => {
      this.refreshing = null;
    });
    return this.refreshing;
  }

  private async fetchManifest() {
    const controller = new AbortController();
    this.request = controller;
    const timeout = setTimeout(() => controller.abort(), MANIFEST_TIMEOUT_MS);
    try {
      const response = await this.fetcher(MANIFEST_URL, {
        cache: "no-store",
        signal: controller.signal,
      });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const next = sortEntries(parseEntries(await response.json()));
      const changed = JSON.stringify(next) !== JSON.stringify(this.entries);
      const recovered = this.manifest !== "ready";
      this.entries = next;
      for (const entry of next) this.known.set(entry.id, entry);
      this.manifest = "ready";
      if (changed || recovered) this.render(false);
    } catch (error) {
      if (this.lifecycle.signal.aborted) return;
      const first = this.manifest === "loading";
      this.manifest = "error";
      console.error("Manifest refresh failed", error);
      this.renderTopbar();
      if (first || !this.entries.length) this.renderContent();
    } finally {
      clearTimeout(timeout);
    }
  }

  private startPolling() {
    clearInterval(this.poll);
    this.poll = setInterval(() => {
      if (!this.doc.hidden) void this.refresh();
    }, POLL_MS);
  }

  // ------------------------------------------------------------ rendering

  private navigate() {
    const route = routeOf(this.win.location.hash);
    const moved = route !== this.route;
    this.route = route;
    this.langOpen = false;
    this.render(false);
    if (moved) {
      this.win.scrollTo?.(0, 0);
      this.content.focus({ preventScroll: true });
    }
  }

  private render(remount: boolean) {
    if (remount) this.mountedKey = null;
    const lang = this.lang === "en" ? "en" : "zh-CN";
    this.doc.documentElement.lang = lang;
    this.renderSidebar();
    this.renderTopbar();
    this.renderContent();
  }

  private applyTheme() {
    const theme =
      this.mode === "system"
        ? this.scheme.matches
          ? "dark"
          : "light"
        : this.mode;
    this.doc.documentElement.setAttribute("data-theme", theme);
  }

  private applyCollapsed() {
    this.app.classList.toggle("bc-app--collapsed", this.collapsed);
    this.sidebar.classList.toggle("bc-sidebar--collapsed", this.collapsed);
  }

  private entry(id: string) {
    return this.entries.find((entry) => entry.id === id);
  }

  private crumbs(): [string, string] {
    const t = this.t;
    if (this.route === "overview") return ["Barracuda", t.overview];
    const entry = this.entry(this.route) ?? this.known.get(this.route);
    return entry
      ? [t.groups[entry.group], entry.title[this.lang]]
      : ["Barracuda", this.route];
  }

  private navItem(id: string, label: string, glyph: Element, current: boolean) {
    return h(
      "a",
      {
        class: "bc-nav-item",
        href: `#${id}`,
        "aria-current": current ? "page" : "false",
        "aria-label": label,
      },
      glyph,
      h("span", { class: "bc-nav-text" }, label),
      h(
        "span",
        { class: "bc-tooltip bc-tooltip--right", "aria-hidden": "true" },
        label,
      ),
    );
  }

  private renderSidebar() {
    const t = this.t;
    this.sidebar.setAttribute("aria-label", t.portal);
    this.sidebar.replaceChildren();
    append(
      this.sidebar,
      h(
        "a",
        { class: "bc-brand", href: "#overview" },
        brandMark(),
        h(
          "span",
          { class: "bc-nav-text" },
          h(
            "span",
            { class: "portal-brand-text" },
            h("span", { class: "portal-brand-name" }, "Barracuda"),
            h("span", { class: "bc-muted portal-brand-sub" }, t.portal),
          ),
        ),
      ),
      h(
        "div",
        { class: "bc-nav-group" },
        this.navItem(
          "overview",
          t.overview,
          icon(ICON_OVERVIEW),
          this.route === "overview",
        ),
      ),
      this.entries.length
        ? h(
            "div",
            { class: "portal-nav-groups" },
            groupsOf(this.entries).map(({ group, entries }) =>
              h(
                "div",
                { class: "bc-nav-group" },
                h("span", { class: "bc-nav-label" }, t.groups[group]),
                entries.map((entry) =>
                  this.navItem(
                    entry.id,
                    entry.title[this.lang],
                    entryIcon(entry),
                    this.route === entry.id,
                  ),
                ),
              ),
            ),
          )
        : null,
      this.plain
        ? h(
            "p",
            { class: "bc-muted bc-sidebar__footer portal-sidebar-footer" },
            h(
              "span",
              { class: "bc-term", tabindex: "0" },
              t.footer,
              h(
                "span",
                { class: "bc-tooltip bc-tooltip--start", role: "tooltip" },
                t.footerTip,
              ),
            ),
          )
        : null,
    );
  }

  private statusBadge() {
    if (this.manifest !== "error") return null;
    return h(
      "span",
      { class: "bc-badge portal-badge--offline", role: "status" },
      icon(ICON_WIFI_OFF, 12),
      this.t.offline,
    );
  }

  private languageMenu() {
    const t = this.t;
    const item = (lang: Lang, label: string) =>
      h(
        "button",
        {
          class: "bc-menu-item",
          type: "button",
          role: "menuitemradio",
          "aria-checked": this.lang === lang ? "true" : "false",
          lang: lang === "zh" ? "zh-CN" : "en",
          onclick: () => this.setLang(lang),
        },
        h(
          "span",
          { class: "portal-menu-check" },
          this.lang === lang ? icon(ICON_CHECK) : null,
        ),
        label,
      );
    return h(
      "div",
      { class: "bc-menu-anchor" },
      h(
        "button",
        {
          class: "bc-button bc-button--ghost bc-button--sm portal-lang",
          type: "button",
          "aria-haspopup": "menu",
          "aria-expanded": this.langOpen ? "true" : "false",
          onclick: () => {
            this.langOpen = !this.langOpen;
            this.renderTopbar();
          },
        },
        icon(ICON_LANGUAGES),
        this.lang === "zh" ? "简体中文" : "English",
        icon(ICON_CHEVRON_DOWN),
      ),
      this.langOpen
        ? h(
            "div",
            { class: "bc-menu", role: "menu", "aria-label": t.language },
            item("zh", "简体中文"),
            item("en", "English"),
          )
        : null,
    );
  }

  private themeControl() {
    const t = this.t;
    const option = (mode: ThemeMode, label: string, glyph: string) =>
      h(
        "button",
        {
          type: "button",
          "aria-pressed": this.mode === mode ? "true" : "false",
          "aria-label": label,
          title: label,
          onclick: () => this.setMode(mode),
        },
        icon(glyph),
      );
    return h(
      "div",
      { class: "bc-segmented", role: "group", "aria-label": t.theme },
      option("light", t.light, ICON_SUN),
      option("dark", t.dark, ICON_MOON),
      option("system", t.system, ICON_MONITOR),
    );
  }

  private phoneTools() {
    const t = this.t;
    const modes: ThemeMode[] = ["system", "light", "dark"];
    return [
      h(
        "button",
        {
          class: "bc-button bc-button--ghost bc-button--sm portal-phone-lang",
          type: "button",
          lang: this.lang === "zh" ? "en" : "zh-CN",
          onclick: () => this.setLang(this.lang === "zh" ? "en" : "zh"),
        },
        t.langButton,
      ),
      h(
        "button",
        {
          class:
            "bc-button bc-button--ghost bc-button--icon portal-phone-theme",
          type: "button",
          "aria-label": t.themeLabel[this.mode],
          onclick: () =>
            this.setMode(modes[(modes.indexOf(this.mode) + 1) % modes.length]),
        },
        icon(
          this.mode === "system"
            ? ICON_MONITOR
            : this.mode === "light"
              ? ICON_SUN
              : ICON_MOON,
          18,
        ),
      ),
    ];
  }

  private renderTopbar() {
    const t = this.t;
    const [section, page] = this.crumbs();
    const label = this.collapsed ? t.expand : t.collapse;
    this.desktopBar.replaceChildren();
    append(
      this.desktopBar,
      h(
        "button",
        {
          class:
            "bc-button bc-button--ghost bc-button--icon bc-sidebar-trigger",
          type: "button",
          "aria-label": label,
          "aria-expanded": this.collapsed ? "false" : "true",
          title: `${label} (Ctrl/⌘ B)`,
          onclick: () => this.toggleSidebar(),
        },
        icon(ICON_PANEL),
      ),
      h("span", { class: "portal-divider" }),
      h("span", { class: "bc-crumb" }, section),
      h("span", { class: "bc-crumb" }, "/"),
      h("span", { class: "bc-crumb--current portal-grow" }, page),
      this.statusBadge(),
      this.languageMenu(),
      this.themeControl(),
    );
    this.phoneBar.replaceChildren();
    append(
      this.phoneBar,
      ...(this.route === "overview"
        ? [
            brandMark(),
            h("span", { class: "portal-grow portal-brand-name" }, "Barracuda"),
          ]
        : [
            h(
              "a",
              {
                class: "bc-button bc-button--ghost portal-back",
                href: "#overview",
              },
              icon(ICON_ARROW_LEFT, 18),
              t.overview,
            ),
            h("span", { class: "portal-grow" }),
          ]),
      this.statusBadge(),
      ...this.phoneTools(),
    );
    this.phoneBar.classList.toggle(
      "portal-topbar--home",
      this.route === "overview",
    );
    this.doc.title =
      this.route === "overview" ? "Barracuda" : `${page} · Barracuda`;
  }

  private status(kind: StatusKind, name?: string) {
    this.content.replaceChildren(
      renderStatus({
        kind,
        lang: this.lang,
        name,
        refresh: () => void this.refresh(),
        back: () => this.go("overview"),
        retry: () => {
          this.mountedKey = null;
          this.renderContent();
        },
      }),
    );
  }

  private leaveModule() {
    this.navigation++;
    this.session.close();
    this.mountedKey = null;
    this.content.removeAttribute("aria-busy");
  }

  private renderContent() {
    if (this.route === "overview") {
      this.leaveModule();
      if (!this.entries.length) {
        if (this.manifest === "loading") this.content.replaceChildren();
        else this.status(this.manifest === "error" ? "manifest" : "empty");
        return;
      }
      const view = {
        entries: this.entries,
        lang: this.lang,
        plain: this.plain,
      };
      this.content.replaceChildren(
        this.phone ? renderPhoneHome(view) : renderOverview(view),
      );
      return;
    }
    const entry = this.entry(this.route);
    if (!entry) {
      this.leaveModule();
      if (this.manifest === "loading") return this.status("loading");
      if (this.manifest === "error" && !this.entries.length)
        return this.status("manifest");
      const known = this.known.get(this.route);
      return this.status(
        "unavailable",
        known ? known.title[this.lang] : this.route,
      );
    }
    const key = JSON.stringify([entry.id, entry.module, this.lang]);
    if (key === this.mountedKey) return;
    void this.mount(entry, key);
  }

  private async mount(entry: WebEntry, key: string) {
    this.leaveModule();
    this.mountedKey = key;
    const navigation = this.navigation;
    const current = () => navigation === this.navigation;
    this.status("loading");
    this.content.setAttribute("aria-busy", "true");
    const root = h("div", { class: "portal-module", "data-entry": entry.id });
    try {
      await this.session.open(
        entry,
        root,
        {
          lang: this.lang,
          toast: (toast) => this.toast(toast),
          navigate: (id) => this.go(id),
        },
        {
          load: this.load,
          onImported: () => {
            if (current()) this.content.replaceChildren(root);
          },
        },
      );
    } catch (error) {
      if (!current()) return;
      this.session.close();
      console.error("Module load failed", error);
      this.status("failed");
    } finally {
      if (current()) this.content.removeAttribute("aria-busy");
    }
  }
}

/** Starts the portal on this page. */
export function startPortal(options?: PortalOptions) {
  return new Portal(options);
}
