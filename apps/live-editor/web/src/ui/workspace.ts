/**
 * The workspace: the top bar, the chat in the middle, and the views to its right. Opening a view
 * adds a column next to the chat; three views (two on a laptop screen) fold the sidebar to its rail.
 */

import type { App, ViewId } from "../app/app";
import { activeChat, currentVersion } from "../state/project";
import { ago, fill, h, icon, iconButton, status, tooltip } from "./dom";
import { ChatColumn } from "./chat";
import { CodePane } from "./code";
import { ChangesPane, ConsolePane, PreviewPane, type Pane } from "./panes";
import { t } from "../i18n";
import {
  collapsed,
  languageMenu,
  phoneBack,
  themeToggle,
  topbar,
  type Menus,
  type Page,
} from "./shell";

const VIEWS: { id: ViewId; icon: Parameters<typeof icon>[0] }[] = [
  { id: "preview", icon: "smartphone" },
  { id: "code", icon: "code-xml" },
  { id: "changes", icon: "git-compare" },
  { id: "console", icon: "square-terminal" },
];

export class Workspace implements Page {
  readonly el: HTMLElement;
  private readonly bar = h("div", { style: { display: "contents" } });
  private readonly work: HTMLElement;
  private readonly handle: HTMLElement;
  private readonly views = h("div", { class: "le-views" });
  private readonly chat: ChatColumn;
  private readonly panes = new Map<ViewId, Pane & { destroy?(): void }>();
  private paneWidth = 440;
  private barKey = "";

  constructor(
    private readonly app: App,
    private readonly menus: Menus,
  ) {
    this.chat = new ChatColumn(app);
    this.handle = h(
      "div",
      {
        class: "le-handle",
        role: "separator",
        "aria-orientation": "vertical",
        "aria-label": t.resizeViews,
        onMouseDown: (event: Event) => this.resize(event as MouseEvent),
      },
      h(
        "span",
        { class: "le-grip", "aria-hidden": "true" },
        icon("grip-vertical"),
      ),
    );
    this.work = h(
      "div",
      { class: "le-work" },
      this.chat.el,
      this.handle,
      this.views,
    );
    this.el = h("div", { style: { display: "contents" } }, this.bar, this.work);
  }

  destroy() {
    for (const pane of this.panes.values()) pane.destroy?.();
  }

  /** Three or more views (two on a laptop screen) need the room. */
  crowded(): boolean {
    const count = this.app.views.length;
    return count >= 3 || (count === 2 && innerWidth < 1400);
  }

  private resize(event: MouseEvent) {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = this.width();
    const count = Math.max(1, this.app.views.length);
    const move = (moved: MouseEvent) => {
      this.paneWidth = Math.min(
        760,
        Math.max(320, startWidth - (moved.clientX - startX) / count),
      );
      this.layout();
    };
    const up = () => {
      removeEventListener("mousemove", move);
      removeEventListener("mouseup", up);
    };
    addEventListener("mousemove", move);
    addEventListener("mouseup", up);
  }

  private width() {
    return innerWidth < 1180 ? Math.min(this.paneWidth, 400) : this.paneWidth;
  }

  private layout() {
    const count = this.app.views.length;
    const side = collapsed(this.app, this) ? "56px" : "var(--sidebar-width)";
    const chatMin = this.crowded() ? 300 : 380;
    const views = `min(${count * this.width()}px, calc(100vw - ${side} - ${chatMin + 1}px))`;
    this.work.style.gridTemplateColumns = count
      ? `minmax(${chatMin}px, 1fr) 1px ${views}`
      : "minmax(0, 1fr)";
    this.handle.hidden = count === 0;
    this.views.hidden = count === 0;
  }

  private pane(id: ViewId): Pane {
    let pane = this.panes.get(id);
    if (!pane) {
      pane =
        id === "preview"
          ? new PreviewPane(this.app, this.menus)
          : id === "code"
            ? new CodePane(this.app, this.menus, (text) =>
                this.chat.draft(text),
              )
            : id === "changes"
              ? new ChangesPane(this.app)
              : new ConsolePane(this.app);
      this.panes.set(id, pane);
    }
    return pane;
  }

  update() {
    const app = this.app;
    const project = app.project();
    if (!project) return;
    this.renderBar();
    this.layout();
    // Views keep their places: the newest opens first, next to the chat.
    for (const [id, pane] of this.panes)
      if (!app.views.includes(id)) {
        pane.destroy?.();
        pane.el.remove();
        this.panes.delete(id);
      }
    app.views.forEach((id, index) => {
      const pane = this.pane(id);
      pane.el.style.order = String(index);
      pane.el.classList.toggle("le-view--first", index === 0);
      pane.el.classList.toggle("le-flash", app.flash === id);
      if (pane.el.parentElement !== this.views) this.views.append(pane.el);
      pane.update();
    });
    this.chat.update();
  }

  private renderBar() {
    const app = this.app;
    const project = app.project();
    if (!project) return;
    const version = currentVersion(project);
    const proposalCount = project.proposal
      ? Object.keys(project.proposal.files).length
      : 0;
    const key = JSON.stringify([
      project.name,
      activeChat(project).title,
      project.versions.map((v) => [v.n, v.title, v.at]),
      version?.n,
      app.build.status,
      app.views,
      app.theme,
      this.menus.open,
      proposalCount,
      app.run.logs.filter((entry) => entry.level === "error").length,
      collapsed(app, this),
    ]);
    if (key === this.barKey) return;
    this.barKey = key;
    const errors = app.run.logs.filter(
      (entry) => entry.level === "error",
    ).length;
    const label = (id: ViewId) =>
      id === "changes" && proposalCount
        ? t.toReview(t.views[id], proposalCount)
        : id === "console" && errors
          ? t.errorCount(t.views[id], errors)
          : t.views[id];
    const built = app.build.status === "ok";
    const failed = app.build.status === "failed";
    fill(
      this.bar,
      topbar(
        app,
        this,
        phoneBack(t.apps, "#/new"),
        h("span", { class: "bc-topbar__divider le-desk" }),
        h("span", { class: "bc-crumb le-wide" }, project.name),
        h("span", { class: "bc-crumb le-wide" }, "/"),
        h(
          "span",
          { class: "bc-crumb--current le-desk le-crumb" },
          t.stored(activeChat(project).title),
        ),
        h("span", { class: "le-grow le-phone" }),
        built &&
          h(
            "span",
            { class: "le-desk" },
            status(
              "success",
              "check",
              h("span", { class: "le-wide" }, t.built),
            ),
          ),
        failed &&
          status(
            "destructive",
            "circle-x",
            h("span", { class: "le-wide" }, t.buildFailed),
          ),
        app.build.status === "building" &&
          status(
            "muted",
            "loader-circle",
            h("span", { class: "le-wide" }, t.building),
          ),
        h("span", { class: "bc-topbar__divider le-desk" }),
        h(
          "span",
          {
            class: "le-row le-views-toggle",
            role: "group",
            "aria-label": t.viewsLabel,
          },
          VIEWS.map((view) =>
            iconButton(
              view.icon,
              label(view.id),
              () => app.toggleView(view.id),
              {
                ghost: true,
                pressed: app.views.includes(view.id),
              },
            ),
          ),
        ),
        h(
          "span",
          { class: "bc-menu-anchor le-desk" },
          iconButton(
            "history",
            t.versions,
            (event) => this.menus.toggle("versions", event, 280),
            {
              ghost: true,
              expanded: this.menus.open === "versions",
            },
          ),
          this.menus.open === "versions" &&
            h(
              "div",
              {
                class: "bc-menu le-menu",
                role: "menu",
                "aria-label": t.versions,
                style: { ...this.menus.side(), width: "280px" },
              },
              [...project.versions].reverse().map((item) =>
                h(
                  "button",
                  {
                    class: "bc-menu-item",
                    type: "button",
                    role: "menuitemradio",
                    "aria-checked": String(item.n === version?.n),
                    onClick: () => {
                      this.menus.close();
                      if (item.n !== version?.n) app.restoreVersion(item.n);
                    },
                  },
                  h(
                    "span",
                    {
                      class: "le-grow",
                      style: { overflow: "hidden", textOverflow: "ellipsis" },
                    },
                    t.stored(item.title),
                  ),
                  h(
                    "span",
                    { class: "bc-mono bc-caption bc-muted" },
                    `v${item.n} · ${ago(item.at)}`,
                  ),
                ),
              ),
              h("p", { class: "le-menu-note" }, t.restoringKeeps),
            ),
        ),
        // On a phone the language and theme stay in Settings and New app; the bar keeps to the views.
        h("span", { class: "le-desk" }, languageMenu(app, this.menus)),
        h("span", { class: "le-desk" }, themeToggle(app)),
        h(
          "a",
          {
            class: "bc-button bc-button--outline bc-button--sm le-install",
            href: `#/app/${project.id}/install`,
            "aria-label": t.installOnDevice,
          },
          icon("upload"),
          h("span", { class: "le-wide" }, t.install),
          tooltip(t.installOnDevice, "le-install-tip"),
        ),
      ),
    );
  }
}
