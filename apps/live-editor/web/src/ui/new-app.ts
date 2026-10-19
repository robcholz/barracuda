/** New app: a fresh conversation. Say what the device should do, or start from an example. */

import type { App } from "../app/app";
import { EXAMPLES } from "../app/templates";
import { DEVICES, type Attachment, type DeviceId } from "../state/project";
import { ago, fill, h, icon, size, svg, tooltip } from "./dom";
import { MARK, SITE } from "./icons";
import { readAttachment } from "./chat";
import {
  resolvedTheme,
  themeToggle,
  topbar,
  type Menus,
  type Page,
} from "./shell";

const IDEAS = ["Show the room temperature", "A kitchen timer with big digits"];

export class NewApp implements Page {
  readonly el: HTMLElement;
  private readonly bar = h("div", { style: { display: "contents" } });
  private readonly figure = h("div", { style: { display: "contents" } });
  private readonly attachmentsEl = h("div", { style: { display: "contents" } });
  private readonly actions = h("div", { class: "bc-composer__actions" });
  private readonly row = h("div", { class: "le-row" });
  private readonly phone = h("div", { class: "bc-frame le-phone-list" });
  private readonly textarea: HTMLTextAreaElement;
  private attachments: Attachment[] = [];
  private device: DeviceId;
  private busy = false;
  private key = "";

  constructor(
    private readonly app: App,
    private readonly menus: Menus,
  ) {
    this.device =
      app.settings.device === "none" ? "round" : app.settings.device;
    this.textarea = h("textarea", {
      rows: 1,
      placeholder: "A kitchen timer: big minutes and seconds, tap to start",
      "aria-label": "Describe the app",
      onInput: () => this.renderActions(),
      onKeyDown: (event: Event) => {
        const key = event as KeyboardEvent;
        if (key.key === "Enter" && !key.shiftKey && !key.isComposing) {
          key.preventDefault();
          void this.send();
        }
      },
    });
    const examples = h(
      "div",
      { class: "bc-frame le-examples", style: { flexDirection: "column" } },
      h(
        "div",
        { class: "bc-list-head" },
        h("span", { class: "bc-list-label" }, "Start from an example"),
      ),
      EXAMPLES.map((example) =>
        h(
          "button",
          {
            class: "bc-list-row",
            type: "button",
            onClick: () => void this.start(example.id, ""),
          },
          icon("app-window", "bc-muted"),
          h("span", { class: "bc-option-title le-grow" }, example.title),
          h("span", { class: "bc-small bc-muted" }, example.text),
          icon("chevron-right", "bc-muted"),
        ),
      ),
    );
    this.el = h(
      "div",
      { style: { display: "contents" } },
      this.bar,
      h(
        "main",
        { class: "le-fresh bc-chat--fresh" },
        h(
          "div",
          { class: "bc-chat-log" },
          h(
            "div",
            { class: "bc-empty", style: { paddingBottom: "var(--space-6)" } },
            this.figure,
            h(
              "div",
              {
                style: { display: "flex", flexDirection: "column", gap: "4px" },
              },
              h("h1", { class: "bc-title", style: { margin: "0" } }, "New app"),
              h(
                "p",
                { class: "bc-small bc-muted", style: { margin: "0" } },
                "Say what the device should do. Barracuda writes the screen and the program, then runs them in the preview.",
              ),
            ),
          ),
        ),
        h(
          "div",
          { class: "bc-chat-dock" },
          h(
            "div",
            { class: "le-column" },
            h(
              "div",
              { class: "bc-composer" },
              this.attachmentsEl,
              h(
                "div",
                { class: "bc-composer__row" },
                this.textarea,
                this.actions,
              ),
            ),
            this.row,
            examples,
            this.phone,
          ),
        ),
      ),
    );
    this.renderActions();
  }

  /** Creates the app from `template` and opens it; with a description, the agent starts on it. */
  private async start(template: string, text: string) {
    if (this.busy) return;
    this.busy = true;
    try {
      await this.app.create(template, text, this.device);
      if (text) this.app.agent?.send(text, this.attachments);
    } catch (error) {
      this.app.notify({
        tone: "error",
        title: "The app could not be created",
        text: String(error),
      });
    } finally {
      this.busy = false;
    }
  }

  private async send() {
    const text = this.textarea.value.trim();
    if (!text) return;
    await this.start("blank", text);
  }

  private renderActions() {
    const input = h("input", {
      class: "le-file",
      type: "file",
      onChange: async (event: Event) => {
        for (const file of [
          ...((event.target as HTMLInputElement).files ?? []),
        ]) {
          const read = await readAttachment(file);
          if (typeof read === "string")
            this.app.notify({ tone: "error", title: read });
          else this.attachments.push(read);
        }
        this.renderAttachments();
      },
    });
    fill(
      this.actions,
      h(
        "label",
        {
          class: "bc-button bc-button--ghost bc-button--icon",
          "aria-label": "Add attachment",
        },
        input,
        icon("paperclip"),
        tooltip("Add attachment"),
      ),
      h(
        "button",
        {
          class: "bc-button bc-button--icon",
          type: "button",
          "aria-label": "Send",
          disabled: !this.textarea.value.trim(),
          onClick: () => void this.send(),
        },
        icon("arrow-up"),
        tooltip("Send"),
      ),
    );
  }

  private renderAttachments() {
    fill(
      this.attachmentsEl,
      this.attachments.length > 0 &&
        h(
          "div",
          { class: "bc-attachments" },
          this.attachments.map((attachment, index) =>
            h(
              "div",
              { class: "bc-attachment bc-attachment--file" },
              icon("file", "bc-muted"),
              h(
                "span",
                { class: "bc-attachment__name" },
                h("span", { class: "bc-small" }, attachment.name),
                h(
                  "span",
                  { class: "bc-caption bc-mono bc-muted" },
                  size(attachment.size),
                ),
              ),
              h(
                "button",
                {
                  class: "bc-icon-button bc-attachment__remove",
                  type: "button",
                  "aria-label": "Remove attachment",
                  onClick: () => {
                    this.attachments.splice(index, 1);
                    this.renderAttachments();
                  },
                },
                icon("x"),
                tooltip("Remove attachment"),
              ),
            ),
          ),
        ),
    );
  }

  update() {
    const app = this.app;
    const theme = resolvedTheme(app.theme);
    const key = JSON.stringify([
      theme,
      app.theme,
      this.device,
      this.menus.open,
      app.apps().map((p) => [p.id, p.name, p.at]),
      app.sidebar,
    ]);
    if (key === this.key) return;
    this.key = key;
    fill(
      this.bar,
      topbar(
        app,
        this,
        h(
          "a",
          {
            class: "bc-brand le-phone",
            href: SITE,
            "aria-label": "Barracuda website",
          },
          svg(MARK),
          h("span", { class: "bc-wordmark" }, "Barracuda"),
        ),
        h("span", { class: "bc-topbar__divider le-desk" }),
        h("span", { class: "bc-crumb--current le-desk" }, "New app"),
        h("span", { class: "le-grow" }),
        themeToggle(app),
      ),
    );
    fill(
      this.figure,
      h("img", {
        class: "bc-figure",
        alt: "",
        src: new URL(
          `ds/assets/${theme === "dark" ? "laptop-dark" : "laptop"}.svg`,
          app.base,
        ).href,
        style: { maxWidth: "200px" },
      }),
    );
    const device = DEVICES[this.device];
    fill(
      this.row,
      h(
        "span",
        { class: "bc-menu-anchor" },
        h(
          "button",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            type: "button",
            "aria-haspopup": "menu",
            "aria-label": "Preview device",
            "aria-expanded": String(this.menus.open === "new-device"),
            onClick: (event: Event) =>
              this.menus.toggle("new-device", event, 240),
          },
          icon("cpu"),
          device.label,
          icon("chevron-down"),
        ),
        this.menus.open === "new-device" &&
          h(
            "div",
            {
              class: "bc-menu le-menu",
              role: "menu",
              "aria-label": "Preview device",
              style: { left: "0", right: "auto", minWidth: "240px" },
            },
            (["round", "square"] as DeviceId[]).map((id) =>
              h(
                "button",
                {
                  class: "bc-menu-item",
                  type: "button",
                  role: "menuitemradio",
                  "aria-checked": String(id === this.device),
                  onClick: () => {
                    this.device = id;
                    this.menus.close();
                  },
                },
                h("span", { class: "le-grow" }, DEVICES[id].label),
                h(
                  "span",
                  { class: "bc-caption bc-muted bc-mono" },
                  DEVICES[id].size,
                ),
              ),
            ),
          ),
      ),
      IDEAS.map((idea) =>
        h(
          "button",
          {
            class: "bc-button bc-button--outline bc-button--sm",
            type: "button",
            onClick: () => {
              this.textarea.value = idea;
              this.renderActions();
              this.textarea.focus();
            },
          },
          idea,
        ),
      ),
    );
    const apps = app.apps();
    fill(
      this.phone,
      h(
        "div",
        { class: "bc-list-head" },
        h("span", { class: "bc-list-label" }, "Your apps"),
      ),
      apps.map((item) =>
        h(
          "a",
          { class: "bc-list-row", href: `#/app/${item.id}` },
          icon("app-window", "bc-muted"),
          h("span", { class: "bc-option-title le-grow" }, item.name),
          h("span", { class: "bc-small bc-muted" }, ago(item.at)),
          icon("chevron-right", "bc-muted"),
        ),
      ),
      h(
        "a",
        { class: "bc-list-row", href: "#/settings" },
        icon("settings", "bc-muted"),
        h("span", { class: "bc-option-title le-grow" }, "Settings"),
        icon("chevron-right", "bc-muted"),
      ),
    );
  }
}
