/**
 * Install on device: the app goes to the device through its portal, in a window next to this
 * one, which writes it where the device runs apps from.
 */

import type { App } from "../app/app";
import {
  browserHost,
  Handoff,
  portalOrigin,
  type Failure,
  type Phase,
} from "../install/handoff";
import { zip } from "../install/zip";
import { manifestOf } from "../state/project";
import { button, fill, h, icon, size } from "./dom";
import {
  phoneBack,
  resolvedTheme,
  themeToggle,
  topbar,
  type Page,
} from "./shell";

/** Which step each phase is on: done, current or still to come. */
const STEPS: Record<Phase, ("done" | "current" | "next")[]> = {
  idle: ["current", "next", "next"],
  opening: ["current", "next", "next"],
  failed: ["current", "next", "next"],
  closed: ["current", "next", "next"],
  ready: ["done", "current", "next"],
  writing: ["done", "done", "current"],
  done: ["done", "done", "done"],
};

function failureText(
  failure: Failure,
  name: string,
): { title: string; text: string } {
  switch (failure.kind) {
    case "blocked":
      return {
        title: "The portal window did not open",
        text: "Allow pop-ups for this site, then open the portal again.",
      };
    case "silent":
      return {
        title: "The portal did not answer",
        text: "Check the address, and that this computer is on the same network as the device.",
      };
    case "refused":
      return {
        title: `${name} was not installed`,
        text: "It was not installed from the portal window. Open the portal again to install it.",
      };
    case "device":
      return {
        title: "The device could not write the app",
        text: failure.message,
      };
  }
}

/** The built app: the files of its `<id>.bapp/` directory. */
interface Built {
  files: Map<string, Uint8Array>;
  version: string;
}

export class InstallPage implements Page {
  readonly el: HTMLElement;
  private readonly bar = h("div", { style: { display: "contents" } });
  private readonly header = h("div", { style: { display: "contents" } });
  private readonly form = h("form", {
    class: "bc-form",
    "aria-label": "Install",
  });
  private readonly handoff: Handoff;
  private address: string;
  private addressError = "";
  private pkg: Built | null = null;
  private pkgError: string | null = null;
  private preparing = true;
  private key = "";

  constructor(private readonly app: App) {
    this.address = app.settings.portal;
    this.handoff = new Handoff(browserHost, () => this.changed());
    this.form.addEventListener("submit", (event) => event.preventDefault());
    this.el = h(
      "div",
      { style: { display: "contents" } },
      this.bar,
      h(
        "div",
        { class: "le-scroll" },
        h("main", { class: "bc-page" }, this.header, this.form),
      ),
    );
    void this.prepare();
  }

  destroy() {
    if (this.handoff.phase === "opening") this.handoff.cancel();
  }

  private changed() {
    this.key = "";
    this.update();
  }

  /** Builds the app as it is, not the proposal under review. */
  private async prepare() {
    const project = this.app.project();
    if (!project) return;
    this.preparing = true;
    const reply = await this.app.buildFiles(project.files);
    if (!reply.ok)
      this.pkgError = `It does not build: ${reply.problem.message}`;
    else
      this.pkg = {
        files: new Map(Object.entries(reply.files)),
        version: String(reply.manifest.version ?? ""),
      };
    this.preparing = false;
    this.changed();
  }

  private open() {
    const project = this.app.project();
    if (!project || !this.pkg) return;
    const origin = portalOrigin(this.address);
    this.addressError = origin
      ? ""
      : "Enter the device's address, like 192.168.1.42:8787";
    if (!origin) return this.changed();
    if (this.address !== this.app.settings.portal)
      void this.app.saveSettings({
        ...this.app.settings,
        portal: this.address.trim(),
      });
    this.handoff.start(origin, {
      id: project.id,
      name: project.name,
      version: this.pkg.version,
      files: this.pkg.files,
    });
  }

  private download() {
    const project = this.app.project();
    if (!project || !this.pkg) return;
    const folder = `${project.id}.bapp`;
    const url = URL.createObjectURL(
      new Blob([zip(folder, this.pkg.files)], { type: "application/zip" }),
    );
    const link = h("a", { href: url, download: `${folder}.zip` });
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    this.app.notify({
      tone: "success",
      title: `${folder}.zip downloaded`,
      text: `Unzip it, then in the device portal under Files make the folder media/apps/${folder} and upload its files there.`,
    });
  }

  update() {
    const app = this.app;
    const project = app.project();
    if (!project) return;
    const theme = resolvedTheme(app.theme);
    const phase = this.handoff.phase;
    const key = JSON.stringify([
      theme,
      app.theme,
      app.sidebar,
      phase,
      this.handoff.failure,
      this.addressError,
      this.preparing,
      this.pkgError,
      project.name,
    ]);
    if (key === this.key) return;
    this.key = key;
    const linked = ["ready", "writing", "done"].includes(phase);
    fill(
      this.bar,
      topbar(
        app,
        this,
        phoneBack(project.name, `#/app/${project.id}`),
        h("span", { class: "bc-topbar__divider le-desk" }),
        h(
          "a",
          {
            class: "bc-crumb le-desk",
            href: `#/app/${project.id}`,
            style: { textDecoration: "none" },
          },
          project.name,
        ),
        h("span", { class: "bc-crumb le-desk" }, "/"),
        h("span", { class: "bc-crumb--current le-desk" }, "Install on device"),
        h("span", { class: "le-grow" }),
        ["ready", "writing"].includes(phase) &&
          h("span", { class: "bc-badge bc-badge--signal" }, "Portal open"),
        phase === "closed" &&
          h(
            "span",
            { class: "bc-badge bc-badge--destructive" },
            "Portal closed",
          ),
        themeToggle(app),
      ),
    );
    fill(
      this.header,
      h(
        "section",
        { class: "bc-header" },
        h(
          "div",
          { class: "bc-header__text" },
          h("h1", { class: "bc-page-title" }, "Install on device"),
          h(
            "p",
            { class: "bc-lead" },
            `${project.name} goes to the device through its portal, in a window next to this one. You install it from there.`,
          ),
        ),
        h(
          "div",
          { class: "bc-header__figure" },
          h("img", {
            class: "bc-figure",
            alt: "",
            src: new URL(
              `ds/assets/${theme === "dark" ? "board-dark" : "board"}.svg`,
              app.base,
            ).href,
            style: { maxWidth: "220px" },
          }),
        ),
      ),
    );

    const states = STEPS[phase];
    const failure =
      phase === "failed" && this.handoff.failure
        ? failureText(this.handoff.failure, project.name)
        : null;
    const row = (title: string, text: string, ...body: unknown[]) =>
      h(
        "div",
        { class: "bc-row" },
        h(
          "div",
          { class: "bc-row__label" },
          h("span", { class: "bc-title" }, title),
          h("span", { class: "bc-small bc-muted" }, text),
        ),
        h("div", { class: "bc-row__body" }, ...(body as never[])),
      );
    const steps = [
      "Open the device portal",
      `Install ${project.name} from the portal window`,
      "The device writes it with its apps",
    ];
    const manifest = manifestOf(project.files);
    const folder = `${project.id}.bapp`;
    const busy = ["opening", "ready", "writing"].includes(phase);
    const total = this.pkg
      ? [...this.pkg.files.values()].reduce(
          (sum, bytes) => sum + bytes.length,
          0,
        )
      : 0;
    fill(
      this.form,
      row(
        "Device",
        "A Barracuda on this computer's network",
        h(
          "div",
          { class: "bc-field" },
          h(
            "label",
            { class: "bc-label", for: "le-address" },
            "Device address",
          ),
          h("input", {
            id: "le-address",
            class: "bc-input bc-input--mono",
            value: this.address,
            placeholder: "192.168.1.42:8787",
            autocomplete: "off",
            spellcheck: "false",
            disabled: busy || linked,
            "aria-invalid": String(!!this.addressError),
            onInput: (event: Event) => {
              this.address = (event.target as HTMLInputElement).value;
            },
          }),
          this.addressError
            ? h("span", { class: "bc-hint bc-hint--error" }, this.addressError)
            : !linked &&
                phase !== "opening" &&
                h(
                  "span",
                  { class: "bc-hint" },
                  "The address the device portal opens at, like ",
                  h("span", { class: "bc-mono" }, "192.168.1.42:8787"),
                ),
        ),
        phase === "opening" &&
          h(
            "span",
            { class: "bc-status bc-muted", role: "status" },
            icon("loader-circle", "bc-spinner"),
            h("span", { class: "bc-shimmer" }, "Waiting for the portal window"),
          ),
        linked &&
          h(
            "dl",
            { class: "bc-kv" },
            h("dt", null, "Portal"),
            h(
              "dd",
              null,
              h(
                "span",
                { class: "bc-mono" },
                portalOrigin(this.address) ?? this.address,
              ),
            ),
          ),
      ),
      row(
        "App",
        "Built in this browser",
        this.preparing
          ? h(
              "span",
              { class: "bc-status bc-muted", role: "status" },
              icon("loader-circle", "bc-spinner"),
              `Building ${project.name}`,
            )
          : this.pkgError
            ? h(
                "div",
                { class: "bc-alert bc-alert--error", role: "alert" },
                icon("circle-alert"),
                h(
                  "div",
                  { class: "bc-alert__body" },
                  h(
                    "div",
                    { class: "bc-alert__title" },
                    `${project.name} cannot be installed`,
                  ),
                  h("div", { class: "bc-small" }, this.pkgError),
                ),
              )
            : this.pkg &&
              h(
                "dl",
                { class: "bc-kv" },
                h("dt", null, "Folder"),
                h(
                  "dd",
                  null,
                  h("span", { class: "bc-mono" }, `media/apps/${folder}`),
                ),
                h("dt", null, "Files"),
                h(
                  "dd",
                  null,
                  h("span", { class: "bc-mono" }, String(this.pkg.files.size)),
                  " · ",
                  h("span", { class: "bc-mono" }, size(total)),
                ),
                h("dt", null, "Version"),
                h(
                  "dd",
                  null,
                  h(
                    "span",
                    { class: "bc-mono" },
                    manifest.version ?? this.pkg.version,
                  ),
                ),
              ),
      ),
      row(
        "Install",
        "You choose Install in the portal window",
        h(
          "ol",
          { class: "bc-steps" },
          steps.map((text, index) =>
            h(
              "li",
              {
                class: `bc-step${states[index] === "done" ? " bc-step--done" : ""}`,
                "aria-current": states[index] === "current" ? "step" : "false",
              },
              h(
                "span",
                { class: "bc-step__mark" },
                states[index] === "done" ? icon("check") : String(index + 1),
              ),
              h(
                "span",
                {
                  class:
                    (index === 1 && phase === "ready") ||
                    (index === 2 && phase === "writing")
                      ? "bc-shimmer"
                      : "",
                },
                text,
              ),
            ),
          ),
        ),
        phase === "ready" &&
          h(
            "span",
            { class: "bc-hint" },
            "The portal window shows what it received. Choose 安装到设备 (Install on device) there.",
          ),
        phase === "writing" &&
          h(
            "div",
            { class: "bc-card bc-card--progress", role: "status" },
            h(
              "div",
              { class: "bc-card__row" },
              icon("loader-circle", "bc-spinner"),
              h(
                "span",
                { class: "bc-option-title le-grow" },
                "Writing ",
                h("span", { class: "bc-mono" }, `media/apps/${folder}`),
              ),
            ),
          ),
        phase === "done" &&
          h(
            "div",
            { class: "bc-card", role: "status" },
            h(
              "div",
              { class: "bc-card__body le-result" },
              h(
                "span",
                { class: "bc-option-title" },
                icon("circle-check", "bc-success"),
                `${project.name} is on the device`,
              ),
              h(
                "span",
                { class: "bc-small bc-muted" },
                "Open it from the device's launcher.",
              ),
            ),
          ),
        failure &&
          h(
            "div",
            { class: "bc-alert bc-alert--error", role: "alert" },
            icon("circle-alert"),
            h(
              "div",
              { class: "bc-alert__body" },
              h("div", { class: "bc-alert__title" }, failure.title),
              h("div", { class: "bc-small" }, failure.text),
            ),
          ),
      ),
      h(
        "div",
        { class: "bc-form__footer" },
        this.pkg &&
          (phase === "failed" || phase === "idle" || phase === "closed") &&
          button(`Download ${folder}.zip`, () => this.download(), {
            kind: "outline",
          }),
        busy &&
          button("Cancel", () => this.handoff.cancel(), { kind: "outline" }),
        phase === "done" &&
          h(
            "a",
            { class: "bc-button", href: `#/app/${project.id}` },
            `Back to ${project.name}`,
          ),
        ["idle", "failed", "closed"].includes(phase) &&
          button(
            phase === "idle" ? "Open device portal" : "Open the portal again",
            () => this.open(),
            {
              disabled: !this.pkg,
            },
          ),
      ),
    );
  }
}
