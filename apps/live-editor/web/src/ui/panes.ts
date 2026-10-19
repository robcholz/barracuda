/** The Preview, Changes and Console views: each a `.bc-pane` the workspace places in a column. */

import type { App } from "../app/app";
import { hunks, type DiffRow } from "../state/diff";
import { DEVICES, type DeviceId } from "../state/project";
import { button, fill, h, icon, iconButton, status, type Child } from "./dom";
import type { Menus } from "./shell";

/** A pane's frame: its bar and its body; the workspace places it and marks the first. */
export interface Pane {
  readonly el: HTMLElement;
  update(): void;
}

function frame(label: string) {
  const bar = h("div", { class: "bc-pane__bar" });
  const body = h("div", { class: "bc-pane__body" });
  const el = h(
    "section",
    { class: "bc-pane le-view", "aria-label": label },
    bar,
    body,
  );
  return { el, bar, body };
}

// ── Preview ─────────────────────────────────────────────────────────

export class PreviewPane implements Pane {
  readonly el: HTMLElement;
  private readonly bar: HTMLElement;
  private readonly body: HTMLElement;
  private readonly canvas = h("canvas", { "aria-hidden": "true" });
  private readonly screen: HTMLElement;
  private readonly overlay = h("div", { style: { display: "contents" } });
  private readonly bezel: HTMLElement;
  private readonly alert = h("div", { style: { display: "contents" } });
  private readonly stage: HTMLElement;
  private readonly footer: HTMLElement;
  private key = "";
  private stopFrames: () => void;

  constructor(
    private readonly app: App,
    private readonly menus: Menus,
  ) {
    const parts = frame("Preview");
    this.el = parts.el;
    this.bar = parts.bar;
    this.body = parts.body;
    this.screen = h(
      "div",
      { class: "le-screen", role: "img" },
      this.canvas,
      this.overlay,
    );
    this.canvas.addEventListener("pointerdown", (event) => {
      const rect = this.canvas.getBoundingClientRect();
      const frame = this.app.frame;
      if (!frame) return;
      app.tap(
        ((event.clientX - rect.left) / rect.width) * frame.width,
        ((event.clientY - rect.top) / rect.height) * frame.height,
      );
    });
    this.bezel = h("div", { class: "le-bezel" }, this.screen);
    this.stage = h("div", { class: "le-stage" });
    this.footer = h("div", { class: "bc-pane__section le-row le-pad" });
    fill(this.body, this.alert, this.stage, this.footer);
    this.stopFrames = app.onFrame(() => this.paint());
  }

  destroy() {
    this.stopFrames();
  }

  private paint() {
    const frame = this.app.frame;
    if (!frame) return;
    const width = frame.width * frame.scale;
    const height = frame.height * frame.scale;
    if (this.canvas.width !== width) this.canvas.width = width;
    if (this.canvas.height !== height) this.canvas.height = height;
    const context = this.canvas.getContext("2d");
    context?.putImageData(
      new ImageData(new Uint8ClampedArray(frame.pixels), width, height),
      0,
      0,
    );
  }

  update() {
    const app = this.app;
    const project = app.project();
    if (!project) return;
    const run = app.run;
    const reviewing = !!project.proposal;
    const device = DEVICES[project.device];
    const failed = app.build.status === "failed";
    const key = JSON.stringify([
      project.device,
      project.name,
      run.state,
      run.reason,
      run.question,
      run.depth,
      failed,
      reviewing,
      app.version,
      app.build.status,
      this.menus.open === "device",
      "app.tsx" in project.files,
    ]);
    if (key === this.key) return;
    this.key = key;

    const badge = failed
      ? h("span", { class: "bc-badge" }, "Last build")
      : run.state === "active"
        ? h("span", { class: "bc-badge bc-badge--signal" }, "Running")
        : run.state === "background"
          ? h("span", { class: "bc-badge" }, "Background")
          : run.state === "starting" || run.state === "idle"
            ? h("span", { class: "bc-badge" }, "Starting")
            : h("span", { class: "bc-badge" }, "Ended");
    fill(
      this.bar,
      h("span", { class: "bc-title le-pane-title" }, "Preview"),
      device.display && badge,
      h("span", { class: "le-grow" }),
      h(
        "span",
        { class: "bc-menu-anchor" },
        h(
          "button",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            type: "button",
            "aria-label": "Preview device",
            "aria-haspopup": "menu",
            "aria-expanded": String(this.menus.open === "device"),
            onClick: (event: Event) => this.menus.toggle("device", event, 240),
          },
          icon("cpu"),
          h("span", { class: "le-wide" }, device.label),
          icon("chevron-down"),
        ),
        this.menus.open === "device" &&
          h(
            "div",
            {
              class: "bc-menu le-menu",
              role: "menu",
              "aria-label": "Preview device",
              style: { ...this.menus.side(), minWidth: "240px" },
            },
            (Object.keys(DEVICES) as DeviceId[]).map((id) =>
              h(
                "button",
                {
                  class: "bc-menu-item",
                  type: "button",
                  role: "menuitemradio",
                  "aria-checked": String(id === project.device),
                  onClick: () => {
                    this.menus.close();
                    app.setDevice(id);
                  },
                },
                h("span", { class: "le-grow" }, DEVICES[id].label),
                h(
                  "span",
                  { class: "bc-mono bc-caption bc-muted" },
                  DEVICES[id].size,
                ),
              ),
            ),
          ),
      ),
      reviewing &&
        h(
          "div",
          {
            class: "bc-segmented bc-segmented--text",
            role: "group",
            "aria-label": "Version",
          },
          h(
            "button",
            {
              type: "button",
              "aria-pressed": String(app.version === "before"),
              onClick: () => app.pickVersion("before"),
            },
            "Before",
          ),
          h(
            "button",
            {
              type: "button",
              "aria-pressed": String(app.version === "after"),
              onClick: () => app.pickVersion("after"),
            },
            "After",
          ),
        ),
      iconButton("rotate-ccw", "Restart app", () => app.restartApp()),
      iconButton("x", "Close Preview", () => app.closeView("preview")),
    );

    fill(
      this.alert,
      failed &&
        h(
          "div",
          { style: { padding: "16px 16px 0" } },
          h(
            "div",
            {
              class: "bc-alert bc-alert--error bc-alert--compact",
              role: "alert",
            },
            icon("circle-alert"),
            h(
              "span",
              { class: "bc-small le-grow" },
              h("span", { class: "bc-alert__title" }, "Build failed"),
              app.build.last ? " · running the last build" : "",
            ),
            button(
              "Show problem",
              () => {
                const problem = app.build.problem;
                app.openFile(problem?.file ?? "app.tsx", problem?.line);
              },
              { kind: "outline", sm: true },
            ),
          ),
        ),
    );

    const hasView = "app.tsx" in project.files;
    if (!device.display && hasView) {
      fill(
        this.stage,
        h(
          "div",
          { class: "bc-empty" },
          h("span", { class: "bc-title" }, "Needs a display"),
          h(
            "span",
            { class: "bc-small bc-muted" },
            `${project.name} has a view. Pick a device with a display to see it.`,
          ),
          button("Use Round 360", () => app.setDevice("round"), {
            kind: "outline",
            sm: true,
          }),
        ),
      );
    } else if (!device.display) {
      fill(
        this.stage,
        h(
          "div",
          { class: "bc-empty" },
          h(
            "span",
            { class: "bc-title" },
            run.state === "ended"
              ? `${project.name} ended`
              : `${project.name} runs headless`,
          ),
          h(
            "span",
            { class: "bc-small bc-muted" },
            "It has no view. Its output is in the Console.",
          ),
          button("Open Console", () => app.show("console"), {
            kind: "outline",
            sm: true,
          }),
        ),
      );
    } else {
      this.bezel.className = `le-bezel${device.display.round ? "" : " le-bezel--square"}`;
      this.screen.setAttribute(
        "aria-label",
        `${project.name} on the preview device`,
      );
      const away = run.state === "background" || run.state === "ended";
      this.canvas.style.visibility =
        run.state === "active" || (run.state === "starting" && app.frame)
          ? "visible"
          : "hidden";
      fill(
        this.overlay,
        (run.state === "idle" || (run.state === "starting" && !app.frame)) && [
          h("span", { class: "le-screen-title" }, project.name),
          h(
            "span",
            { class: "le-screen-text" },
            app.build.status === "failed" && !app.build.last
              ? "Build failed"
              : "Starting",
          ),
        ],
        away && [
          h(
            "span",
            { class: "le-screen-text" },
            run.state === "background"
              ? `${project.name} is in the background`
              : (run.reason ?? `${project.name} ended`),
          ),
          h(
            "button",
            {
              class: "le-key le-key--lime",
              type: "button",
              onClick: () =>
                run.state === "background"
                  ? app.lifecycle("foreground")
                  : app.restartApp(),
            },
            run.state === "background" ? "Open" : "Start",
          ),
        ],
        run.question &&
          h(
            "div",
            {
              class: "le-ask",
              role: "dialog",
              "aria-label": run.question.title,
            },
            h("span", { class: "le-screen-title" }, run.question.title),
            run.question.body &&
              h("span", { class: "le-screen-text" }, run.question.body),
            h(
              "span",
              { style: { display: "flex", gap: "8px", position: "relative" } },
              h(
                "button",
                {
                  class: "le-key le-key--gray",
                  type: "button",
                  onClick: () => app.answer(false),
                },
                run.question.deny || "Deny",
              ),
              h(
                "button",
                {
                  class: "le-key le-key--lime",
                  type: "button",
                  onClick: () => app.answer(true),
                },
                run.question.allow || "Allow",
              ),
            ),
          ),
      );
      fill(this.stage, this.bezel);
      this.paint();
    }

    const live = run.state === "active" && !!device.display;
    fill(
      this.footer,
      button("Back", () => app.lifecycle("back"), {
        kind: "outline",
        sm: true,
        disabled: !live || run.depth === 0,
      }),
      button("Background", () => app.lifecycle("background"), {
        kind: "outline",
        sm: true,
        disabled: !live,
      }),
      button("Terminate", () => app.lifecycle("terminate"), {
        kind: "outline",
        sm: true,
        disabled: run.state === "ended" || run.state === "idle",
      }),
      h("span", { class: "le-grow" }),
      button("Reset storage", () => app.resetStorage(), {
        kind: "ghost",
        sm: true,
      }),
    );
  }
}

// ── Changes ─────────────────────────────────────────────────────────

/** A diff row in the `.le-diff` grid. */
function diffRow(row: DiffRow): Child[] {
  if ("hunk" in row) return [h("span", { class: "hunk" }, row.hunk)];
  const cls = row.kind === "+" ? "add" : row.kind === "-" ? "del" : "";
  return [
    h("span", { class: `n ${cls}` }, row.old ?? ""),
    h("span", { class: `n ${cls}` }, row.new ?? ""),
    h(
      "span",
      { class: `s ${cls}` },
      row.kind === " " ? " " : row.kind === "+" ? "+" : "−",
    ),
    h("span", { class: cls }, row.text),
  ];
}

export class ChangesPane implements Pane {
  readonly el: HTMLElement;
  private readonly bar: HTMLElement;
  private readonly body: HTMLElement;
  private closed = new Set<string>();
  private key = "";

  constructor(private readonly app: App) {
    const parts = frame("Changes");
    this.el = parts.el;
    this.bar = parts.bar;
    this.body = parts.body;
  }

  update() {
    const app = this.app;
    const project = app.project();
    if (!project) return;
    const proposal = project.proposal;
    const key = JSON.stringify([proposal, project.files, [...this.closed]]);
    if (key === this.key) return;
    this.key = key;
    const summary = app.proposalSummary();
    const add = summary.reduce((sum, file) => sum + file.add, 0);
    const del = summary.reduce((sum, file) => sum + file.del, 0);
    fill(
      this.bar,
      h(
        "span",
        { class: "le-bar-text le-pane-title" },
        h("span", { class: "bc-title" }, "Changes"),
        proposal &&
          h(
            "span",
            { class: "bc-caption bc-muted" },
            h("span", { class: "bc-mono" }, String(summary.length)),
            ` file${summary.length === 1 ? "" : "s"} · `,
            h("span", { class: "bc-mono bc-success" }, `+${add}`),
            " ",
            h("span", { class: "bc-mono bc-destructive" }, `−${del}`),
          ),
      ),
      proposal &&
        button("Discard", () => app.discardChanges(), {
          kind: "outline",
          sm: true,
        }),
      proposal &&
        button("Apply", () => app.applyChanges(), { sm: true, icon: "check" }),
      iconButton("x", "Close Changes", () => app.closeView("changes")),
    );
    if (!proposal) {
      fill(
        this.body,
        h(
          "div",
          {
            class: "bc-empty",
            style: { flex: "1 1 auto", justifyContent: "center" },
          },
          h("span", { class: "bc-title" }, "Nothing to review"),
          h(
            "span",
            { class: "bc-small bc-muted" },
            "Changes Barracuda proposes wait here until you apply them.",
          ),
        ),
      );
      return;
    }
    fill(
      this.body,
      h(
        "div",
        { class: "le-stack" },
        summary.map((file) => {
          const open = !this.closed.has(file.name);
          const after = proposal.files[file.name];
          return h(
            "section",
            { class: "bc-card", "aria-label": file.name },
            h(
              "button",
              {
                class: "bc-fold",
                type: "button",
                "aria-expanded": String(open),
                onClick: () => {
                  if (open) this.closed.add(file.name);
                  else this.closed.delete(file.name);
                  this.update();
                },
              },
              icon("file-code", "bc-muted"),
              h(
                "span",
                { class: "bc-tool__name le-grow" },
                file.name,
                after === null
                  ? " (deleted)"
                  : !(file.name in project.files)
                    ? " (new)"
                    : "",
              ),
              file.add > 0 &&
                h(
                  "span",
                  { class: "bc-mono bc-caption bc-success" },
                  `+${file.add}`,
                ),
              file.del > 0 &&
                h(
                  "span",
                  { class: "bc-mono bc-caption bc-destructive" },
                  `−${file.del}`,
                ),
              icon("chevron-right", "bc-muted"),
            ),
            open &&
              h(
                "div",
                { class: "le-diff" },
                hunks(project.files[file.name] ?? "", after ?? "").flatMap(
                  diffRow,
                ),
              ),
          );
        }),
      ),
    );
  }
}

// ── Console ─────────────────────────────────────────────────────────

const pad = (n: number) => String(n).padStart(2, "0");
const clock = (at: number) => {
  const date = new Date(at);
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
};

export class ConsolePane implements Pane {
  readonly el: HTMLElement;
  private readonly bar: HTMLElement;
  private readonly body: HTMLElement;
  private level: "all" | "warn" | "error" = "all";
  private copied = false;
  private key = "";

  constructor(private readonly app: App) {
    const parts = frame("Console");
    this.el = parts.el;
    this.bar = parts.bar;
    this.body = parts.body;
  }

  update() {
    const app = this.app;
    const project = app.project();
    if (!project) return;
    const logs = app.run.logs;
    const key = JSON.stringify([
      logs.length,
      logs.at(-1)?.at,
      this.level,
      this.copied,
      project.storage,
    ]);
    if (key === this.key) return;
    this.key = key;
    const count = (level: typeof this.level) =>
      logs.filter((entry) => level === "all" || entry.level === level).length;
    const shown = logs.filter(
      (entry) => this.level === "all" || entry.level === this.level,
    );
    const levels: [typeof this.level, string][] = [
      ["all", "All"],
      ["warn", "Warnings"],
      ["error", "Errors"],
    ];
    fill(
      this.bar,
      h("span", { class: "bc-title le-pane-title" }, "Console"),
      h("span", { class: "le-grow" }),
      h(
        "div",
        {
          class: "bc-segmented bc-segmented--text",
          role: "group",
          "aria-label": "Level",
        },
        levels.map(([id, label]) =>
          h(
            "button",
            {
              type: "button",
              "aria-pressed": String(this.level === id),
              onClick: () => {
                this.level = id;
                this.update();
              },
            },
            label,
            h(
              "span",
              { class: "bc-mono bc-muted", style: { marginLeft: "6px" } },
              String(count(id)),
            ),
          ),
        ),
      ),
      iconButton("ban", "Clear console", () => app.clearLog()),
      iconButton("x", "Close Console", () => app.closeView("console")),
    );
    const entry = project.files["app.json"]
      ? (JSON.parse(project.files["app.json"]) as { entry?: string }).entry
      : undefined;
    const log = h(
      "ol",
      { class: "le-logs", "aria-label": "Console output" },
      shown.map((item) =>
        h(
          "li",
          {
            class: `le-log${item.level !== "log" ? ` le-log--${item.level}` : ""}${item.source === "system" ? " le-log--system" : ""}`,
          },
          h("span", { class: "le-log__time" }, clock(item.at)),
          h("span", { class: "le-log__level" }, item.level),
          h("span", null, item.text),
        ),
      ),
    );
    const copyLabel = this.copied ? "Copied" : "Copy output";
    fill(
      this.body,
      h(
        "div",
        { class: "le-stack" },
        h(
          "div",
          { class: "bc-window", "data-theme": "dark" },
          h(
            "div",
            { class: "bc-window__bar" },
            h(
              "span",
              { class: "bc-window__title" },
              `${project.id} · ${entry ?? "main.lua"}`,
            ),
            iconButton(this.copied ? "check" : "copy", copyLabel, () => {
              void navigator.clipboard?.writeText(
                shown
                  .map((item) => `${clock(item.at)} ${item.level} ${item.text}`)
                  .join("\n"),
              );
              this.copied = true;
              this.update();
              setTimeout(() => {
                this.copied = false;
                this.update();
              }, 1500);
            }),
          ),
          shown.length
            ? log
            : h(
                "pre",
                { class: "bc-window__code bc-muted" },
                "No console output.",
              ),
        ),
        h(
          "div",
          { class: "bc-window" },
          h(
            "div",
            { class: "bc-window__bar" },
            h("span", { class: "bc-window__title" }, "storage · kv"),
          ),
          h(
            "pre",
            { class: "bc-window__code" },
            JSON.stringify(project.storage.kv, null, 2),
          ),
        ),
        Object.keys(project.storage.files).length > 0 &&
          h(
            "div",
            { class: "bc-window" },
            h(
              "div",
              { class: "bc-window__bar" },
              h("span", { class: "bc-window__title" }, "storage · files"),
            ),
            h(
              "pre",
              { class: "bc-window__code" },
              Object.keys(project.storage.files).sort().join("\n"),
            ),
          ),
      ),
    );
    const last = this.body.querySelector(".le-logs");
    last?.lastElementChild?.scrollIntoView({ block: "nearest" });
  }
}

export { status };
