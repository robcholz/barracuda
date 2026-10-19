/**
 * Install on device: the app goes to the device through its portal, in a window next to this
 * one, and the person at the device installs it.
 */

import type { App } from "../app/app";
import {
  browserHost,
  Handoff,
  portalOrigin,
  type Failure,
  type Phase,
} from "../install/handoff";
import { shortSigner, signingKey } from "../install/key";
import { pack, packageFiles } from "../package";
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
  sending: ["done", "current", "next"],
  waiting: ["done", "done", "current"],
  declined: ["done", "done", "current"],
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
        title: `${name} was not sent`,
        text: "The portal window closed without sending it. Open the portal again to send it.",
      };
    case "device":
      return { title: "The device refused the package", text: failure.message };
  }
}

interface Package {
  bytes: Uint8Array;
  signer: string;
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
  private pkg: Package | null = null;
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

  /** Builds the app as it is (not the proposal under review) and packs it, signed. */
  private async prepare() {
    const project = this.app.project();
    if (!project) return;
    this.preparing = true;
    const reply = await this.app.buildFiles(project.files);
    if (!reply.ok) {
      this.pkgError = `It does not build: ${reply.problem.message}`;
    } else {
      try {
        const key = await signingKey();
        const files = new Map(Object.entries(reply.files));
        this.pkg = {
          bytes: await pack(packageFiles(files), key),
          signer: await shortSigner(key),
          version: String(reply.manifest.version ?? ""),
        };
      } catch (error) {
        this.pkgError = error instanceof Error ? error.message : String(error);
      }
    }
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
      bytes: this.pkg.bytes,
    });
  }

  private download() {
    const project = this.app.project();
    if (!project || !this.pkg) return;
    const url = URL.createObjectURL(
      new Blob([this.pkg.bytes.slice().buffer], {
        type: "application/octet-stream",
      }),
    );
    const link = h("a", { href: url, download: `${project.id}.bapp` });
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    this.app.notify({
      tone: "success",
      title: `${project.id}.bapp downloaded`,
      text: "Upload it in the device portal under Files, into media/apps, then install it under Apps.",
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
    const linked = ["ready", "sending", "waiting", "declined", "done"].includes(
      phase,
    );
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
        ["ready", "sending", "waiting"].includes(phase) &&
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
            `${project.name} goes to the device through its portal, in a window next to this one. The person at the device installs it.`,
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
      `Open the device portal`,
      `Send ${project.name} from the portal window`,
      "The person at the device installs it",
    ];
    const manifest = manifestOf(project.files);
    const file = `${project.id}.bapp`;
    const busy = ["opening", "ready", "sending", "waiting"].includes(phase);
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
        "Package",
        "Built and signed in this browser",
        this.preparing
          ? h(
              "span",
              { class: "bc-status bc-muted", role: "status" },
              icon("loader-circle", "bc-spinner"),
              "Building the package",
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
                    `${project.name} cannot be packed`,
                  ),
                  h("div", { class: "bc-small" }, this.pkgError),
                ),
              )
            : this.pkg &&
              h(
                "dl",
                { class: "bc-kv" },
                h("dt", null, "File"),
                h(
                  "dd",
                  null,
                  h("span", { class: "bc-mono" }, file),
                  " · ",
                  h("span", { class: "bc-mono" }, size(this.pkg.bytes.length)),
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
                h("dt", null, "Signer"),
                h(
                  "dd",
                  null,
                  h("span", { class: "bc-mono" }, this.pkg.signer),
                  " · ",
                  h(
                    "span",
                    { class: "bc-term", tabindex: "0" },
                    "your key",
                    h(
                      "span",
                      {
                        class: "bc-tooltip bc-tooltip--start",
                        role: "tooltip",
                      },
                      `The device takes updates to ${project.name} only from this key`,
                    ),
                  ),
                ),
              ),
      ),
      row(
        "Install",
        "You send it; the person at the device decides",
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
                    (index === 2 && phase === "waiting")
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
            "The portal window shows what it received. Choose Send to device there.",
          ),
        phase === "sending" &&
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
                "Sending ",
                h("span", { class: "bc-mono" }, file),
              ),
            ),
          ),
        phase === "waiting" &&
          h(
            "span",
            { class: "bc-hint" },
            "Tap Install on the device's screen. A device without a screen asks in its portal, under Apps, from a phone on its setup hotspot.",
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
        phase === "declined" &&
          h(
            "div",
            { class: "bc-card", role: "status" },
            h(
              "div",
              { class: "bc-card__body le-result" },
              h(
                "span",
                { class: "bc-option-title" },
                icon("circle-x", "bc-muted"),
                "Declined on the device",
              ),
              h(
                "span",
                { class: "bc-small bc-muted" },
                "Send it again when the person at the device is ready.",
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
          button(`Download ${file}`, () => this.download(), {
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
        ["idle", "failed", "closed", "declined"].includes(phase) &&
          button(
            phase === "idle"
              ? "Open device portal"
              : phase === "declined"
                ? "Send again"
                : "Open the portal again",
            () => this.open(),
            {
              disabled: !this.pkg,
            },
          ),
      ),
    );
  }
}
