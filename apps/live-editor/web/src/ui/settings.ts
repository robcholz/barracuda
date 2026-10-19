/** Settings: the model service, how the agent's changes reach the code, and the open app's manifest. */

import type { App, Provider, Settings as SettingsData } from "../app/app";
import { manifestOf } from "../state/project";
import { button, fill, h, tooltip } from "./dom";
import { lang, t } from "../i18n";
import {
  languageMenu,
  phoneBack,
  themeToggle,
  topbar,
  type Menus,
  type Page,
} from "./shell";

const providers = (): { id: Provider; title: string; hint: string }[] => [
  { id: "anthropic", title: "Anthropic", hint: t.anthropicHint },
  { id: "openai", title: t.openaiTitle, hint: t.openaiHint },
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
    "aria-label": t.settings,
  });
  private draft: Draft;
  private saved: Draft;
  private showKey = false;
  private errors: Partial<Record<"key" | "url" | "name" | "version", string>> =
    {};
  private barKey = "";

  constructor(
    private readonly app: App,
    private readonly menus: Menus,
  ) {
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
              h("h1", { class: "bc-page-title" }, t.settings),
              h("p", { class: "bc-lead" }, t.settingsLead),
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
        errors.url = t.urlError;
      }
    }
    const project = this.app.project();
    if (project) {
      if (!d.name.trim() || [...d.name].length > 64) errors.name = t.chars(64);
      if (!d.version.trim() || [...d.version].length > 32)
        errors.version = t.chars(32);
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
    this.app.notify({ tone: "success", title: t.settingsSaved });
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
        t.discardEdits,
        () => {
          this.draft = { ...this.saved };
          this.errors = {};
          this.renderForm();
        },
        { kind: "outline", disabled: clean },
      ),
      button(t.saveSettings, () => void this.save(), { disabled: clean }),
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
        t.modelService,
        t.modelServiceText,
        h(
          "div",
          {
            class: "le-cards",
            role: "radiogroup",
            "aria-label": t.modelService,
          },
          providers().map((provider) =>
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
            t.baseUrl,
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
          t.apiKey,
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
                placeholder: t.pasteKey,
              },
            ),
            button(
              this.showKey ? t.hide : t.show,
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
            t.keptHere,
            h(
              "span",
              { class: "bc-tooltip bc-tooltip--start", role: "tooltip" },
              t.sentOnlyTo,
            ),
          ),
        ),
        this.field(
          "le-model",
          t.model,
          this.input(
            "le-model",
            model,
            (value) =>
              this.set(openai ? "openaiModel" : "anthropicModel", value),
            {
              class: "bc-input bc-input--mono",
              placeholder: openai ? t.modelId : "claude-opus-5-5",
            },
          ),
        ),
      ),
      row(
        t.changes,
        t.changesText,
        this.switchRow("le-review", t.review, t.reviewText, d.review, () =>
          this.set("review", !d.review, true),
        ),
        this.switchRow("le-tap", t.tap, t.tapText, d.tap, () =>
          this.set("tap", !d.tap, true),
        ),
      ),
      project &&
        row(
          project.name,
          t.manifest,
          this.field(
            "le-name",
            t.appName,
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
              t.appId,
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
                t.fixed,
                h(
                  "span",
                  { class: "bc-tooltip bc-tooltip--start", role: "tooltip" },
                  t.fixedText,
                ),
              ),
            ),
            this.field(
              "le-version",
              t.version,
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
              h("span", { class: "bc-label", id: "le-shape" }, t.shape),
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
                  t.round,
                ),
                h(
                  "button",
                  {
                    type: "button",
                    "aria-pressed": String(d.shape === "rect"),
                    onClick: () => this.set("shape", "rect", true),
                  },
                  t.rectangle,
                ),
              ),
              h(
                "span",
                { class: "bc-hint" },
                d.shape === "round" ? t.roundText : t.rectText,
              ),
            ),
        ),
      this.footer,
    );
    this.renderFooter();
  }

  update() {
    const app = this.app;
    const key = JSON.stringify([lang, app.theme, app.sidebar, this.menus.open]);
    if (key === this.barKey) return;
    this.barKey = key;
    fill(
      this.bar,
      topbar(
        app,
        this,
        phoneBack(t.apps, "#/new"),
        h("span", { class: "bc-topbar__divider le-desk" }),
        h("span", { class: "bc-crumb--current le-desk" }, t.settings),
        h("span", { class: "le-grow" }),
        languageMenu(app, this.menus),
        themeToggle(app),
      ),
    );
  }
}

export { tooltip };
