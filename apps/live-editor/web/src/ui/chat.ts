/**
 * The chat column: the conversation with the agent and the composer. The log keeps its newest
 * message in view until the person scrolls up themselves.
 */

import type { App } from "../app/app";
import {
  activeChat,
  type Attachment,
  type ChatItem,
  type Step,
  type ToolCall,
} from "../state/project";
import { MARK_ICON } from "./icons";
import {
  button,
  fill,
  h,
  icon,
  iconButton,
  size,
  svg,
  tooltip,
  type Child,
} from "./dom";
import { markdown } from "./markdown";
import { t } from "../i18n";

const MAX_ATTACHMENT_BYTES = 512 * 1024;

/** Reads a picked file as an attachment: text as text, images as base64. */
export async function readAttachment(file: File): Promise<Attachment | string> {
  if (file.size > MAX_ATTACHMENT_BYTES) return t.tooLarge(file.name);
  if (file.type.startsWith("image/")) {
    const bytes = new Uint8Array(await file.arrayBuffer());
    let binary = "";
    for (const byte of bytes) binary += String.fromCharCode(byte);
    return {
      name: file.name,
      type: file.type,
      size: file.size,
      data: btoa(binary),
    };
  }
  return {
    name: file.name,
    type: file.type || "text/plain",
    size: file.size,
    text: await file.text(),
  };
}

/** What a tool card shows in its one line: the call's main argument. */
export function toolArgs(call: ToolCall): string {
  const input = call.input;
  if (typeof input.path === "string") return input.path;
  if (typeof input.x === "number" && typeof input.y === "number")
    return `${input.x}, ${input.y}`;
  const first = Object.values(input).find(
    (value) => typeof value === "string" || typeof value === "number",
  );
  return first === undefined ? "" : String(first);
}

export class ChatColumn {
  readonly el: HTMLElement;
  private readonly log: HTMLElement;
  private readonly thread: HTMLElement;
  private readonly textarea: HTMLTextAreaElement;
  private readonly bar: HTMLElement;
  private readonly attachmentsEl: HTMLElement;
  private readonly actions: HTMLElement;
  private attachments: Attachment[] = [];
  private replyTo: { id: string; text: string } | null = null;
  private open = new Set<string>();
  private copied: string | null = null;
  private stick = true;
  private userScrolledAt = 0;
  private key = "";

  constructor(private readonly app: App) {
    this.thread = h("div", { class: "le-thread" });
    const intent = () => {
      this.userScrolledAt = Date.now();
    };
    this.log = h(
      "div",
      {
        class: "bc-chat-log",
        onWheel: intent,
        onTouchMove: intent,
        onPointerDown: intent,
        onKeyDown: intent,
        onScroll: () => {
          // Only the person's own scrolling unpins the conversation from its newest message.
          if (Date.now() - this.userScrolledAt > 600) return;
          const log = this.log;
          this.stick = log.scrollHeight - log.scrollTop - log.clientHeight < 48;
        },
      },
      this.thread,
    );
    new ResizeObserver(() => {
      if (this.stick) this.log.scrollTop = this.log.scrollHeight;
    }).observe(this.thread);

    this.textarea = h("textarea", {
      rows: 1,
      placeholder: t.askBarracuda,
      "aria-label": t.message,
      onInput: () => {
        this.grow();
        this.renderActions();
      },
      onKeyDown: (event: Event) => {
        const key = event as KeyboardEvent;
        if (key.key === "Enter" && !key.shiftKey && !key.isComposing) {
          key.preventDefault();
          this.send();
        }
      },
    });
    this.bar = h("div", { style: { display: "contents" } });
    this.attachmentsEl = h("div", { style: { display: "contents" } });
    this.actions = h("div", { class: "bc-composer__actions" });
    const composer = h(
      "div",
      { class: "bc-composer" },
      this.bar,
      this.attachmentsEl,
      h("div", { class: "bc-composer__row" }, this.textarea, this.actions),
    );
    this.el = h(
      "section",
      { class: "le-chat", "aria-label": t.chat },
      this.log,
      h(
        "div",
        { class: "bc-chat-dock" },
        h("div", { class: "le-thread le-dock-thread" }, composer),
      ),
    );
    this.renderComposer();
  }

  /** Puts text in the composer, as the problem card's "Ask Barracuda to fix" does. */
  draft(text: string) {
    this.textarea.value = text;
    this.grow();
    this.renderActions();
    this.textarea.focus();
  }

  private grow() {
    this.textarea.style.height = "auto";
    this.textarea.style.height = `${Math.min(this.textarea.scrollHeight, 200)}px`;
  }

  private running(): boolean {
    return !!this.app.agent?.running;
  }

  private send() {
    const text = this.textarea.value.trim();
    if (!text || this.running() || !this.app.agent) return;
    this.app.agent.send(text, this.attachments, this.replyTo?.id);
    this.textarea.value = "";
    this.attachments = [];
    this.replyTo = null;
    this.stick = true;
    this.grow();
    this.renderComposer();
  }

  private renderComposer() {
    fill(
      this.bar,
      this.replyTo &&
        h(
          "div",
          { class: "bc-composer__bar" },
          icon("reply", "bc-muted"),
          h("span", { class: "bc-muted" }, t.replyToBarracuda),
          h("span", { class: "bc-composer__text" }, this.replyTo.text),
          iconButton("x", t.cancelReply, () => {
            this.replyTo = null;
            this.renderComposer();
          }),
        ),
    );
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
              iconButton(
                "x",
                t.removeAttachment,
                () => {
                  this.attachments.splice(index, 1);
                  this.renderComposer();
                },
                { class: "bc-attachment__remove" },
              ),
            ),
          ),
        ),
    );
    this.renderActions();
  }

  private renderActions() {
    const running = this.running();
    const input = h("input", {
      class: "le-file",
      type: "file",
      onChange: async (event: Event) => {
        const target = event.target as HTMLInputElement;
        for (const file of [...(target.files ?? [])]) {
          const read = await readAttachment(file);
          if (typeof read === "string")
            this.app.notify({ tone: "error", title: read });
          else this.attachments.push(read);
        }
        this.renderComposer();
      },
    });
    fill(
      this.actions,
      h(
        "label",
        {
          class: "bc-button bc-button--ghost bc-button--icon",
          "aria-label": t.addAttachment,
        },
        input,
        icon("paperclip"),
        tooltip(t.addAttachment),
      ),
      running
        ? h(
            "button",
            {
              class: "bc-button bc-button--icon",
              type: "button",
              "aria-label": t.stop,
              onClick: () => this.app.agent?.stop(),
            },
            icon("circle-stop"),
            tooltip(t.stop),
          )
        : h(
            "button",
            {
              class: "bc-button bc-button--icon",
              type: "button",
              "aria-label": t.send,
              disabled: !this.textarea.value.trim() || !this.app.agent,
              onClick: () => this.send(),
            },
            icon("arrow-up"),
            tooltip(t.send),
          ),
    );
  }

  update() {
    const project = this.app.project();
    if (!project) return;
    const chat = activeChat(project);
    const key = JSON.stringify([
      chat.id,
      chat.items,
      [...this.open],
      this.copied,
      this.running(),
      project.proposal?.turn,
    ]);
    if (key === this.key) return;
    const switched = !this.key.startsWith(`["${chat.id}"`);
    this.key = key;
    if (switched) this.stick = true;
    fill(
      this.thread,
      chat.items.length
        ? chat.items.map((item) => this.item(item))
        : this.empty(project.name),
    );
    this.renderActions();
    if (this.stick) this.log.scrollTop = this.log.scrollHeight;
  }

  private empty(name: string) {
    return h(
      "div",
      { class: "bc-empty" },
      h("span", { class: "bc-title" }, name),
      h("span", { class: "bc-small bc-muted" }, t.chatEmpty),
    );
  }

  private toggle(id: string) {
    if (this.open.has(id)) this.open.delete(id);
    else this.open.add(id);
    this.key = "";
    this.update();
  }

  private item(item: ChatItem): Child {
    if (item.kind === "note")
      return h(
        "div",
        { role: "separator", class: "bc-separator" },
        t.stored(item.text),
      );
    if (item.kind === "user")
      return h(
        "div",
        { class: "bc-turn bc-turn--user" },
        item.attachments.length > 0 &&
          h(
            "div",
            { class: "bc-attachments" },
            item.attachments.map((attachment) =>
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
              ),
            ),
          ),
        h("p", { class: "bc-bubble" }, item.text),
        h("span", { class: "bc-meta" }, t.sent),
      );
    const reply = item.steps
      .map((step) => step.text)
      .filter(Boolean)
      .join("\n\n");
    const empty = item.steps.every(
      (step) => !step.text && !step.tools.length && !step.thinking,
    );
    return h(
      "div",
      { class: "bc-turn" },
      h(
        "span",
        { class: "bc-meta bc-meta--author" },
        svg(MARK_ICON),
        "Barracuda",
      ),
      item.steps.map((step, index) => this.step(item.id, index, step)),
      item.status === "running" &&
        empty &&
        h("span", { class: "bc-shimmer bc-small" }, t.thinking),
      item.status === "running" &&
        !empty &&
        h("span", { class: "bc-shimmer bc-small" }, t.working),
      item.status === "stopped" &&
        h(
          "span",
          { class: "bc-status bc-muted" },
          icon("circle-stop"),
          t.stopped,
        ),
      item.status === "error" &&
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
            item.error ?? t.serviceFailed,
          ),
          button(t.settings, () => (location.hash = "#/settings"), {
            kind: "outline",
            sm: true,
          }),
        ),
      item.changes && this.changes(item.id, item.changes),
      item.status !== "running" &&
        reply &&
        h(
          "span",
          { class: "bc-meta", style: { gap: "4px" } },
          iconButton(
            this.copied === item.id ? "check" : "copy",
            this.copied === item.id ? t.copied : t.copy,
            () => {
              void navigator.clipboard?.writeText(reply);
              this.copied = item.id;
              this.key = "";
              this.update();
              setTimeout(() => {
                this.copied = null;
                this.key = "";
                this.update();
              }, 1500);
            },
          ),
          iconButton("reply", t.reply, () => {
            this.replyTo = { id: item.id, text: reply.slice(0, 120) };
            this.renderComposer();
            this.textarea.focus();
          }),
        ),
    );
  }

  private step(turn: string, index: number, step: Step): Child[] {
    const out: Child[] = [];
    if (step.thinking) {
      const id = `${turn}/${index}/thought`;
      const open = this.open.has(id);
      out.push(
        h(
          "button",
          {
            class: "bc-fold-link",
            type: "button",
            "aria-expanded": String(open),
            onClick: () => this.toggle(id),
          },
          t.thoughtFor(
            h(
              "span",
              { class: "bc-mono" },
              String(Math.max(1, Math.round(step.thinking.ms / 1000))),
            ),
          ),
          icon("chevron-right"),
        ),
        open && step.thinking.text
          ? h("p", { class: "bc-quote" }, step.thinking.text)
          : null,
      );
    }
    if (step.text) out.push(markdown(step.text));
    for (const call of step.tools) out.push(this.tool(call));
    return out;
  }

  private tool(call: ToolCall): Child {
    const open = this.open.has(call.id);
    const state =
      call.status === "running"
        ? h(
            "span",
            { class: "bc-status bc-muted" },
            icon("loader-circle", "bc-spinner"),
            t.running,
          )
        : call.status === "error"
          ? h(
              "span",
              { class: "bc-status bc-destructive" },
              icon("circle-x"),
              t.failed,
            )
          : h(
              "span",
              { class: "bc-status bc-success" },
              icon("check"),
              t.succeeded,
            );
    const rows: [string, string][] = Object.entries(call.input)
      .filter(([key]) => key !== "content")
      .map(([key, value]) => [
        key,
        typeof value === "string" ? value : JSON.stringify(value),
      ]);
    if (typeof call.input.content === "string")
      rows.push(["content", t.lines(call.input.content.split("\n").length)]);
    if (call.output)
      rows.push([
        "result",
        call.output.length > 600
          ? `${call.output.slice(0, 600)}…`
          : call.output,
      ]);
    return h(
      "div",
      { class: "bc-card" },
      h(
        "button",
        {
          class: "bc-fold",
          type: "button",
          "aria-expanded": String(open),
          onClick: () => this.toggle(call.id),
        },
        icon("wrench", "bc-muted"),
        h("span", { class: "bc-tool__name" }, t.tools[call.name] ?? call.name),
        h("span", { class: "bc-tool__args" }, toolArgs(call)),
        state,
        icon("chevron-right", "bc-muted"),
      ),
      open &&
        h(
          "dl",
          { class: "bc-card__body bc-kv bc-kv--dense" },
          rows.map(([key, value]) => [
            h("dt", null, key),
            h("dd", { class: "bc-mono" }, value),
          ]),
        ),
    );
  }

  private changes(
    turn: string,
    changes: NonNullable<Extract<ChatItem, { kind: "agent" }>["changes"]>,
  ): Child {
    const count = changes.files.length;
    const label =
      changes.state === "applied"
        ? h("span", { class: "bc-status bc-success" }, icon("check"), t.applied)
        : changes.state === "discarded"
          ? h("span", { class: "bc-status bc-muted" }, t.discarded)
          : h("span", { class: "bc-status bc-muted" }, t.waitingForReview);
    const project = this.app.project();
    const reviewing =
      changes.state === "review" && project?.proposal?.turn === turn;
    return h(
      "section",
      { class: "bc-card", "aria-label": t.views.changes },
      h(
        "div",
        { class: "bc-card__head" },
        icon("git-compare", "bc-muted"),
        h("h3", { class: "bc-title le-grow" }, t.filesChanged(count)),
        label,
      ),
      changes.files.map((file) =>
        h(
          "div",
          { class: "bc-card__row" },
          h("span", { class: "bc-mono bc-small le-grow" }, file.name),
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
        ),
      ),
      h(
        "div",
        { class: "bc-card__foot" },
        reviewing
          ? button(t.reviewChanges, () => this.app.show("changes"), {
              kind: "outline",
              sm: true,
            })
          : changes.state === "applied" &&
              button(
                t.openCode,
                () => this.app.openFile(changes.files[0]?.name ?? "app.tsx"),
                { kind: "outline", sm: true },
              ),
      ),
    );
  }
}
