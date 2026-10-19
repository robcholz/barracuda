/** Settings: the model service, how the agent's changes reach the code, and the open app's manifest. */

import type { App, Provider, Settings as SettingsData } from "../app/app";
import { manifestOf } from "../state/project";
import { button, fill, h, tooltip } from "./dom";
import { phoneBack, themeToggle, topbar, type Page } from "./shell";

const PROVIDERS: { id: Provider; title: string; hint: string }[] = [
  { id: "anthropic", title: "Anthropic", hint: "Your Anthropic API key" },
  {
    id: "openai",
    title: "OpenAI compatible",
    hint: "Any service with the same API",
  },
];

interface Draft extends SettingsData {
  name: string;
  version: string;
  shape: "round" | "rect";
}

export class SettingsPage implements Page {
  readonly el: HTMLElement;
  private readonly bar = h("div", { style: { display: "contents" } });
  private readonly form = h("form", {
    class: "bc-form",
    "aria-label": "Settings",
  });
  private draft: Draft;
  private saved: Draft;
  private showKey = false;
  private errors: Partial<Record<"key" | "url" | "name" | "version", string>> =
    {};
  private barKey = "";

  constructor(private readonly app: App) {
    this.saved = this.load();
    this.draft = { ...this.saved };
    this.form.addEventListener("submit", (event) => event.preventDefault());
    this.el = h(
      "div",
      { style: { display: "contents" } },
      this.bar,
      h(
        "div",
        { class: "le-scroll" },
        h(
          "main",
          { class: "bc-page" },
          h(
            "section",
            { class: "bc-header" },
            h(
              "div",
              { class: "bc-header__text" },
              h("h1", { class: "bc-page-title" }, "Settings"),
              h(
                "p",
                { class: "bc-lead" },
                "The model Barracuda writes apps with, how its changes reach your code, and the open app's manifest.",
              ),
            ),
          ),
          this.form,
        ),
      ),
    );
    this.renderForm();
  }

  private load(): Draft {
    const project = this.app.project();
    const manifest = project ? manifestOf(project.files) : {};
    return {
      ...this.app.settings,
      name: manifest.name ?? "",
      version: manifest.version ?? "",
      shape: manifest.view?.shape === "rect" ? "rect" : "round",
    };
  }

  private dirty() {
    return JSON.stringify(this.draft) !== JSON.stringify(this.saved);
  }

  private set<K extends keyof Draft>(key: K, value: Draft[K], render = false) {
    this.draft = { ...this.draft, [key]: value };
    this.errors = {};
    if (render) this.renderForm();
    else this.renderFooter();
  }

  private validate(): boolean {
    const d = this.draft;
    const errors: typeof this.errors = {};
    if (d.provider === "openai") {
      try {
        const url = new URL(d.openaiBaseUrl);
        if (!/^https?:$/.test(url.protocol)) throw new Error();
      } catch {
        errors.url =
          "Enter the service's base URL, like https://api.example.com/v1";
      }
    }
    const project = this.app.project();
    if (project) {
      if (!d.name.trim() || [...d.name].length > 64)
        errors.name = "Use 1–64 characters";
      if (!d.version.trim() || [...d.version].length > 32)
        errors.version = "Use 1–32 characters";
    }
    this.errors = errors;
    return Object.keys(errors).length === 0;
  }

  private async save() {
    if (!this.validate()) return this.renderForm();
    const { name, version, shape, ...settings } = this.draft;
    await this.app.saveSettings(settings);
    const project = this.app.project();
    if (project) {
      const before = manifestOf(project.files);
      if (
        before.name !== name ||
        before.version !== version ||
        (before.view?.shape ?? "round") !== shape
      )
        this.app.setManifest({
          name: name.trim(),
          version: version.trim(),
          shape,
        });
    }
    this.saved = { ...this.draft };
    this.renderForm();
    this.app.notify({ tone: "success", title: "Settings saved" });
  }

  private field(
    id: string,
    label: string,
    control: HTMLElement,
    error?: string,
    hint?: HTMLElement | string,
  ) {
    return h(
      "div",
      { class: "bc-field" },
      h("label", { class: "bc-label", for: id }, label),
      control,
      error
        ? h("span", { class: "bc-hint bc-hint--error" }, error)
        : hint
          ? h("span", { class: "bc-hint" }, hint)
          : null,
    );
  }

  private input(
    id: string,
    value: string,
    onInput: (value: string) => void,
    options: Record<string, string | boolean> = {},
  ) {
    return h("input", {
      id,
      class: "bc-input",
      value,
      spellcheck: "false",
      ...options,
      onInput: (event: Event) =>
        onInput((event.target as HTMLInputElement).value),
    });
  }

  private switchRow(
    id: string,
    title: string,
    hint: string,
    checked: boolean,
    toggle: () => void,
  ) {
    return h(
      "label",
      { class: "bc-switch-row" },
      h(
        "span",
        null,
        h("span", { id, class: "bc-option-title" }, title),
        h("span", { class: "bc-hint" }, hint),
      ),
      h("button", {
        class: "bc-switch",
        role: "switch",
        "aria-checked": String(checked),
        "aria-labelledby": id,
        type: "button",
        onClick: toggle,
      }),
    );
  }

  private footer = h("div", { class: "bc-form__footer" });

  private renderFooter() {
    const clean = !this.dirty();
    fill(
      this.footer,
      button(
        "Discard edits",
        () => {
          this.draft = { ...this.saved };
          this.errors = {};
          this.renderForm();
        },
        { kind: "outline", disabled: clean },
      ),
      button("Save settings", () => void this.save(), { disabled: clean }),
    );
  }

  private renderForm() {
    const d = this.draft;
    const openai = d.provider === "openai";
    const project = this.app.project();
    const row = (
      title: string,
      text: string,
      ...body: (HTMLElement | null | false)[]
    ) =>
      h(
        "div",
        { class: "bc-row" },
        h(
          "div",
          { class: "bc-row__label" },
          h("span", { class: "bc-title" }, title),
          h("span", { class: "bc-small bc-muted" }, text),
        ),
        h("div", { class: "bc-row__body" }, ...body),
      );
    const key = openai ? d.openaiKey : d.anthropicKey;
    const model = openai ? d.openaiModel : d.anthropicModel;
    fill(
      this.form,
      row(
        "Model service",
        "Where the editor's agent runs",
        h(
          "div",
          {
            class: "le-cards",
            role: "radiogroup",
            "aria-label": "Model service",
          },
          PROVIDERS.map((provider) =>
            h(
              "label",
              { class: "bc-radio-card" },
              h("input", {
                type: "radio",
                name: "le-provider",
                checked: d.provider === provider.id,
                onChange: () => this.set("provider", provider.id, true),
              }),
              h(
                "span",
                null,
                h("span", { class: "bc-option-title" }, provider.title),
                h("span", { class: "bc-hint" }, provider.hint),
              ),
            ),
          ),
        ),
        openai &&
          this.field(
            "le-url",
            "API Base URL",
            this.input(
              "le-url",
              d.openaiBaseUrl,
              (value) => this.set("openaiBaseUrl", value),
              {
                class: "bc-input bc-input--mono",
                type: "url",
                placeholder: "https://api.example.com/v1",
                "aria-invalid": String(!!this.errors.url),
              },
            ),
            this.errors.url,
          ),
        this.field(
          "le-key",
          "API Key",
          h(
            "div",
            { class: "le-inline" },
            this.input(
              "le-key",
              key,
              (value) => this.set(openai ? "openaiKey" : "anthropicKey", value),
              {
                class: "bc-input bc-input--mono",
                type: this.showKey ? "text" : "password",
                autocomplete: "new-password",
                placeholder: "Paste your key",
              },
            ),
            button(
              this.showKey ? "Hide" : "Show",
              () => {
                this.showKey = !this.showKey;
                this.renderForm();
              },
              { kind: "outline" },
            ),
          ),
          undefined,
          h(
            "span",
            { class: "bc-term", tabindex: "0" },
            "Kept in this browser",
            h(
              "span",
              { class: "bc-tooltip bc-tooltip--start", role: "tooltip" },
              "Sent only to the model service",
            ),
          ),
        ),
        this.field(
          "le-model",
          "Model",
          this.input(
            "le-model",
            model,
            (value) =>
              this.set(openai ? "openaiModel" : "anthropicModel", value),
            {
              class: "bc-input bc-input--mono",
              placeholder: openai ? "Model ID" : "claude-opus-5-5",
            },
          ),
        ),
      ),
      row(
        "Changes",
        "How Barracuda's edits reach your code",
        this.switchRow(
          "le-review",
          "Review before applying",
          "Proposed changes wait in Changes until you apply them",
          d.review,
          () => this.set("review", !d.review, true),
        ),
        this.switchRow(
          "le-tap",
          "Let Barracuda tap the preview",
          "It checks its own changes on the preview device",
          d.tap,
          () => this.set("tap", !d.tap, true),
        ),
      ),
      project &&
        row(
          project.name,
          "The open app's manifest",
          this.field(
            "le-name",
            "App name",
            this.input("le-name", d.name, (value) => this.set("name", value), {
              "aria-invalid": String(!!this.errors.name),
            }),
            this.errors.name,
          ),
          h(
            "div",
            { class: "le-pair" },
            this.field(
              "le-id",
              "App ID",
              h("input", {
                id: "le-id",
                class: "bc-input bc-input--mono",
                value: project.id,
                disabled: true,
              }),
              undefined,
              h(
                "span",
                { class: "bc-term", tabindex: "0" },
                "Fixed",
                h(
                  "span",
                  { class: "bc-tooltip bc-tooltip--start", role: "tooltip" },
                  "Devices know the app by its ID",
                ),
              ),
            ),
            this.field(
              "le-version",
              "Version",
              this.input(
                "le-version",
                d.version,
                (value) => this.set("version", value),
                {
                  class: "bc-input bc-input--mono",
                  "aria-invalid": String(!!this.errors.version),
                },
              ),
              this.errors.version,
            ),
          ),
          "app.tsx" in project.files &&
            h(
              "div",
              { class: "bc-field" },
              h("span", { class: "bc-label", id: "le-shape" }, "Display shape"),
              h(
                "div",
                {
                  class: "bc-segmented bc-segmented--text",
                  role: "group",
                  "aria-labelledby": "le-shape",
                  style: { alignSelf: "flex-start" },
                },
                h(
                  "button",
                  {
                    type: "button",
                    "aria-pressed": String(d.shape === "round"),
                    onClick: () => this.set("shape", "round", true),
                  },
                  "Round",
                ),
                h(
                  "button",
                  {
                    type: "button",
                    "aria-pressed": String(d.shape === "rect"),
                    onClick: () => this.set("shape", "rect", true),
                  },
                  "Rectangle",
                ),
              ),
              h(
                "span",
                { class: "bc-hint" },
                d.shape === "round"
                  ? "Keeps to the inscribed circle, so it fits every display"
                  : "Uses the corners, so it needs a rectangular display",
              ),
            ),
        ),
      this.footer,
    );
    this.renderFooter();
  }

  update() {
    const app = this.app;
    const key = JSON.stringify([app.theme, app.sidebar]);
    if (key === this.barKey) return;
    this.barKey = key;
    fill(
      this.bar,
      topbar(
        app,
        this,
        phoneBack("Apps", "#/new"),
        h("span", { class: "bc-topbar__divider le-desk" }),
        h("span", { class: "bc-crumb--current le-desk" }, "Settings"),
        h("span", { class: "le-grow" }),
        themeToggle(app),
      ),
    );
  }
}

export { tooltip };
