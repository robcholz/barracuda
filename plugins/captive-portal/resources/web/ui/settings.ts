import type { Lang, PortalContext } from "../src/contract";
import { deviceError, type DeviceError } from "./device";
import { h, icon, pick, type Children, type Text } from "./dom";
import { fitColumns } from "./grid";
import {
  ICON_CHEVRON_RIGHT,
  ICON_EXTERNAL_LINK,
  ICON_EYE,
  ICON_EYE_OFF,
} from "./icons";
import { row } from "./layout";
import { KIT_STRINGS } from "./strings";

interface FieldBase {
  /** The JSON key in the request body. */
  name: string;
  label: Text;
  hint?: Text;
  /** Empty optional fields are left out of the body; required ones block submission. */
  optional?: boolean;
  /**
   * A button on the input's line, after it (and after 显示/隐藏): an action on this one value, such
   * as 「验证」 or 「测试连接」. Text, URL and secret fields only.
   */
  action?: Node;
}
export interface TextField extends FieldBase {
  kind: "text" | "url";
  value?: string;
  placeholder?: Text;
  /** Machine values are mono (default). */
  mono?: boolean;
}
export interface SecretField extends FieldBase {
  kind: "secret";
  placeholder?: Text;
}
export interface NumberField extends FieldBase {
  kind: "number";
  value?: number;
  /** Shown in an addon after the input (`ms`, `tokens`, `bytes`), never in the label. */
  unit?: string;
  min?: number;
  max?: number;
}
export interface SwitchField extends FieldBase {
  kind: "switch";
  value?: boolean;
}
export interface RadioField extends FieldBase {
  kind: "radio";
  options: readonly {
    value: string;
    label: Text;
    hint?: Text;
    /** SVG markup for the 40px tile (an `ICON_*` or `MARK_*` constant). */
    icon?: string;
  }[];
  value?: string;
  /** Cards per line (default 2). */
  columns?: number;
}
export type Field =
  TextField | SecretField | NumberField | SwitchField | RadioField;

export interface SettingsRow {
  title: Text;
  hint?: Text;
  /** An external link under the hint (「打开 @BotFather」), opened in a new tab. */
  link?: { label: Text; href: string };
  fields: readonly Field[];
  /** Anything after the fields in the row body: a result card, a QR Code (see `./blocks`). */
  blocks?: Children;
}

export type Values = Record<string, string | number | boolean>;

export interface SettingsFormOptions {
  /** Where the form posts. Never shown on the page. */
  endpoint: string;
  rows: readonly SettingsRow[];
  /** The 「高级」 fold: defaults the user rarely changes. */
  advanced?: {
    hint?: Text;
    fields: readonly Field[];
    open?: boolean;
    /** Lay the fields out in this many columns instead of one. */
    columns?: number;
    /** Runs when the person opens or closes the fold. */
    onToggle?: (open: boolean) => void;
  };
  /** The submit button: a verb naming the result (「注册模型」, 「保存并替换通道」). */
  submit: Text;
  /** Turns the values into the request body; defaults to the values object. */
  body?: (values: Values) => unknown;
  /** Replaces parts of the success toast, for example an action that navigates. */
  success?: {
    title?: Text;
    body?: Text;
    action?: { label: Text; run(): void };
  };
  /** Runs after the device accepted the body. */
  onSuccess?: (values: Values) => void;
  /** Runs on a non-2xx reply, before the toast; see {@link SubmitOptions.onError}. */
  onError?: SubmitOptions["onError"];
}

export interface SettingsForm {
  element: HTMLFormElement;
  /** Validates and returns the values, or `null` (and marks the fields) when something is wrong. */
  values(): Values | null;
  /** Marks a field invalid with a message (and a machine `code` in mono after it), or clears it with `null`. */
  setError(name: string, message: string | null, code?: string): void;
  /** The footer (「清空」, submit), for pages that show it only in some states. */
  footer: HTMLElement;
  /** Validates and posts; resolves with the outcome (also reported by toast). */
  submit(): Promise<SubmitOutcome>;
}

interface Control {
  field: Field;
  read(): string | number | boolean | undefined;
  validate(): string | null;
  setError(message: string | null, code?: string): void;
  reset(): void;
  focus(): void;
  clearSecret(): void;
}

let uid = 0;

export type SubmitOutcome = "accepted" | "rejected" | "failed" | "aborted";

export interface SubmitOptions {
  endpoint: string;
  body: unknown;
  method?: "POST" | "PUT" | "DELETE";
  /** Default 15 s; the request is also aborted with the module. */
  timeoutMs?: number;
  success?: SettingsFormOptions["success"];
  /** Called by the 「重试」 action of the no-reply toast. */
  retry?: () => void;
  /**
   * Runs on a non-2xx reply with the device's `{error, message, code}` body, before the toast; return
   * a `title` or `body` to replace the toast's (for example `{ body: error.message }`). A `body` is
   * shown for a 4xx only: a 5xx message describes the device's internals.
   */
  onError?: (error: DeviceError) => { title?: Text; body?: Text } | void;
}

/**
 * Sends JSON and reports the outcome through `context.toast`: 2xx accepted, 4xx rejected (status in
 * mono), anything else failed; no reply (network error or timeout) offers a retry. Never toasts once
 * the module is gone.
 */
export async function submitJson(
  context: PortalContext,
  options: SubmitOptions,
): Promise<SubmitOutcome> {
  const s = KIT_STRINGS[context.lang];
  const lang = context.lang;
  const request = new AbortController();
  const cancel = () => request.abort();
  context.signal.addEventListener("abort", cancel, { once: true });
  const timeout = setTimeout(cancel, options.timeoutMs ?? 15_000);
  try {
    const response = await fetch(options.endpoint, {
      method: options.method ?? "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(options.body),
      signal: request.signal,
      cache: "no-store",
      redirect: "error",
    });
    if (context.signal.aborted) return "aborted";
    if (response.ok) {
      const success = options.success ?? {};
      context.toast({
        kind: "success",
        title: success.title ? pick(success.title, lang) : s.accepted,
        body: success.body ? pick(success.body, lang) : undefined,
        action: success.action && {
          label: pick(success.action.label, lang),
          run: success.action.run,
        },
      });
      return "accepted";
    }
    const error = await deviceError(response);
    if (context.signal.aborted) return "aborted";
    const custom = options.onError?.(error) || {};
    const client = response.status >= 400 && response.status < 500;
    // a 5xx message describes the device's internals, so only a 4xx one reaches the reader
    const unreachable = !client && error.error === "upstream_unavailable";
    context.toast({
      kind: "error",
      title: custom.title
        ? pick(custom.title, lang)
        : response.status === 404
          ? s.missing
          : client
            ? s.rejected
            : unreachable
              ? s.unreachable
              : s.failed,
      body:
        client && custom.body
          ? pick(custom.body, lang)
          : unreachable
            ? s.unreachableHint
            : undefined,
      code: String(response.status),
    });
    return response.status >= 400 && response.status < 500
      ? "rejected"
      : "failed";
  } catch {
    if (context.signal.aborted) return "aborted";
    context.toast({
      kind: "error",
      title: s.noReply,
      action: options.retry && { label: s.retry, run: options.retry },
    });
    return "failed";
  } finally {
    clearTimeout(timeout);
    context.signal.removeEventListener("abort", cancel);
  }
}

function labelText(field: Field, lang: Lang, forId?: string) {
  const s = KIT_STRINGS[lang];
  const label = forId
    ? h("label", { class: "bc-label", for: forId }, pick(field.label, lang))
    : h("span", { class: "bc-label" }, pick(field.label, lang));
  if (field.optional)
    label.append(
      h(
        "span",
        { class: "bc-optional" },
        lang === "zh" ? `（${s.optional}）` : ` (${s.optional})`,
      ),
    );
  return label;
}

function errorLine() {
  const line = h("span", { class: "bc-hint bc-hint--error", role: "alert" });
  line.hidden = true;
  return line;
}

function hintLine(field: Field, lang: Lang) {
  return field.hint && field.kind !== "switch"
    ? h("span", { class: "bc-hint" }, pick(field.hint, lang))
    : null;
}

/** Builds one control. Returned `element` goes into a row body; `control` drives validation and values. */
export function fieldControl(
  field: Field,
  lang: Lang,
): { element: HTMLElement; control: Control } {
  const s = KIT_STRINGS[lang];
  const id = `kit-${field.name}-${++uid}`;
  const required = () => s.required(pick(field.label, lang));
  const showError = (
    line: HTMLElement,
    inputs: HTMLElement[],
    message: string | null,
    code?: string,
  ) => {
    line.replaceChildren(
      message ?? "",
      message && code ? " · " : "",
      message && code ? h("span", { class: "bc-mono" }, code) : "",
    );
    line.hidden = !message;
    for (const input of inputs) {
      if (message) input.setAttribute("aria-invalid", "true");
      else input.removeAttribute("aria-invalid");
      if (message) input.setAttribute("aria-describedby", `${id}-error`);
      else input.removeAttribute("aria-describedby");
    }
  };

  switch (field.kind) {
    case "text":
    case "url":
    case "secret": {
      const secret = field.kind === "secret";
      const input = h("input", {
        id,
        class: `bc-input${secret || field.mono !== false ? " bc-input--mono" : ""}`,
        type: secret ? "password" : field.kind,
        name: field.name,
        autocomplete: secret ? "new-password" : "off",
        spellcheck: "false",
        required: !field.optional,
        placeholder: field.placeholder
          ? pick(field.placeholder, lang)
          : undefined,
      });
      if (!secret && field.value !== undefined)
        input.defaultValue = field.value;
      const line = errorLine();
      line.id = `${id}-error`;
      let element: HTMLElement;
      let reveal: HTMLButtonElement | null = null;
      if (secret) {
        input.style.cssText = "flex:1 1 auto;min-width:0";
        const glyph = () =>
          icon(input.type === "password" ? ICON_EYE : ICON_EYE_OFF);
        reveal = h("button", {
          class: "bc-button bc-button--outline",
          type: "button",
          "aria-controls": id,
          "aria-pressed": "false",
        });
        const paint = () => {
          reveal!.replaceChildren(
            glyph(),
            input.type === "password" ? s.show : s.hide,
          );
          reveal!.setAttribute(
            "aria-pressed",
            input.type === "password" ? "false" : "true",
          );
        };
        reveal.addEventListener("click", () => {
          input.type = input.type === "password" ? "text" : "password";
          paint();
        });
        paint();
      }
      if (secret || field.action) {
        // on a narrow screen the buttons wrap under a full-width input
        input.style.cssText = "flex:1 1 280px;min-width:0";
        const line2 = h("div", null, input, reveal, field.action);
        line2.style.cssText = "display:flex;flex-wrap:wrap;gap:var(--space-2)";
        element = h(
          "div",
          { class: "bc-field" },
          labelText(field, lang, id),
          line2,
          hintLine(field, lang),
          line,
        );
      } else {
        element = h(
          "label",
          { class: "bc-field" },
          labelText(field, lang),
          input,
          hintLine(field, lang),
          line,
        );
      }
      input.addEventListener("input", () => showError(line, [input], null));
      return {
        element,
        control: {
          field,
          read: () => {
            const value = secret ? input.value : input.value.trim();
            return value === "" && field.optional ? undefined : value;
          },
          validate: () => {
            const value = secret ? input.value : input.value.trim();
            if (!value) return field.optional ? null : required();
            if (
              field.kind === "url" &&
              !/^https?:\/\/[^\s/?#]+[^\s]*$/i.test(value)
            )
              return s.url;
            return null;
          },
          setError: (message, code) => showError(line, [input], message, code),
          reset: () => {
            input.value = input.defaultValue;
            if (reveal && input.type !== "password") reveal.click();
            showError(line, [input], null);
          },
          focus: () => input.focus(),
          clearSecret: () => {
            if (secret) input.value = "";
          },
        },
      };
    }
    case "number": {
      const min = field.min ?? 0,
        max = field.max ?? 4294967295;
      const input = h("input", {
        id,
        class: "bc-input bc-input--mono",
        type: "number",
        name: field.name,
        inputmode: "numeric",
        min,
        max,
        step: 1,
        required: !field.optional,
      });
      if (field.value !== undefined) input.defaultValue = String(field.value);
      const group = h(
        "span",
        { class: "bc-input-group" },
        input,
        field.unit ? h("span", { class: "bc-addon" }, field.unit) : null,
      );
      const line = errorLine();
      line.id = `${id}-error`;
      // a number needs no more than a short box, as the design draws it
      const element = h(
        "label",
        { class: "bc-field", style: "max-width: 280px" },
        labelText(field, lang),
        group,
        hintLine(field, lang),
        line,
      );
      input.addEventListener("input", () => showError(line, [input], null));
      const parse = () => Number(input.value.trim());
      return {
        element,
        control: {
          field,
          read: () =>
            input.value.trim() === "" && field.optional ? undefined : parse(),
          validate: () => {
            const text = input.value.trim();
            if (!text) return field.optional ? null : required();
            const value = parse();
            return Number.isInteger(value) && value >= min && value <= max
              ? null
              : s.number(min, max);
          },
          setError: (message, code) => showError(line, [input], message, code),
          reset: () => {
            input.value = input.defaultValue;
            showError(line, [input], null);
          },
          focus: () => input.focus(),
          clearSecret: () => {},
        },
      };
    }
    case "switch": {
      const initial = field.value ?? false;
      const toggle = h("button", {
        class: "bc-switch",
        type: "button",
        role: "switch",
        name: field.name,
        "aria-checked": String(initial),
        "aria-labelledby": `${id}-label`,
      });
      toggle.addEventListener("click", () =>
        toggle.setAttribute(
          "aria-checked",
          toggle.getAttribute("aria-checked") === "true" ? "false" : "true",
        ),
      );
      const element = h(
        "label",
        { class: "bc-switch-row" },
        h(
          "span",
          null,
          h(
            "span",
            { id: `${id}-label`, class: "bc-option-title" },
            pick(field.label, lang),
          ),
          field.hint
            ? h("span", { class: "bc-hint" }, pick(field.hint, lang))
            : null,
        ),
        toggle,
      );
      return {
        element,
        control: {
          field,
          read: () => toggle.getAttribute("aria-checked") === "true",
          validate: () => null,
          setError: () => {},
          reset: () => toggle.setAttribute("aria-checked", String(initial)),
          focus: () => toggle.focus(),
          clearSecret: () => {},
        },
      };
    }
    case "radio": {
      const initial = field.value ?? field.options[0]?.value ?? "";
      const inputs = field.options.map((option) =>
        h("input", {
          type: "radio",
          name: field.name,
          value: option.value,
          checked: option.value === initial,
        }),
      );
      const cards = field.options.map((option, index) => {
        const text = h(
          "span",
          null,
          h("span", { class: "bc-option-title" }, pick(option.label, lang)),
          option.hint
            ? h("span", { class: "bc-hint" }, pick(option.hint, lang))
            : null,
        );
        text.style.cssText = "flex:1 1 auto;min-width:0";
        if (!option.icon)
          return h("label", { class: "bc-radio-card" }, inputs[index], text);
        const tile = h("span", { class: "bc-option-icon" });
        const template = document.createElement("template");
        template.innerHTML = option.icon.startsWith("<svg")
          ? option.icon
          : `<svg class="bc-icon" viewBox="0 0 24 24" aria-hidden="true">${option.icon}</svg>`;
        tile.append(template.content);
        return h(
          "label",
          { class: "bc-radio-card bc-radio-card--icon" },
          tile,
          text,
          inputs[index],
        );
      });
      const group = h(
        "div",
        { role: "radiogroup", "aria-label": pick(field.label, lang) },
        cards,
      );
      group.style.cssText = `display:grid;grid-template-columns:${fitColumns(field.columns ?? 2, 180, "var(--space-2)")};gap:var(--space-2)`;
      return {
        element: group,
        control: {
          field,
          read: () => inputs.find((input) => input.checked)?.value ?? "",
          validate: () =>
            inputs.some((input) => input.checked) || field.optional
              ? null
              : required(),
          setError: () => {},
          reset: () => {
            for (const input of inputs) input.checked = input.value === initial;
          },
          focus: () =>
            (inputs.find((input) => input.checked) ?? inputs[0])?.focus(),
          clearSecret: () => {},
        },
      };
    }
  }
}

/**
 * A settings form with label-left rows, an optional 「高级」 fold and a footer holding 「清空」 and the
 * submit button. Submitting validates every field (red border and
 * message), then posts JSON with {@link submitJson}. Secrets are cleared after the device accepts them
 * and when the module goes away.
 */
export function settingsForm(
  options: SettingsFormOptions,
  context: PortalContext,
): SettingsForm {
  const { lang } = context;
  const s = KIT_STRINGS[lang];
  const controls: Control[] = [];
  const build = (field: Field) => {
    const { element, control } = fieldControl(field, lang);
    controls.push(control);
    return element;
  };
  const rows = options.rows.map((section) => {
    const line = row(
      section.title,
      section.hint,
      lang,
      section.fields.map(build),
      section.blocks,
    );
    if (section.link) {
      const link = h(
        "a",
        {
          class: "bc-link bc-small",
          href: section.link.href,
          target: "_blank",
          rel: "noreferrer",
        },
        pick(section.link.label, lang),
        icon(ICON_EXTERNAL_LINK),
      );
      link.style.cssText = "display:inline-flex;align-items:center;gap:4px";
      line.firstElementChild!.append(link);
    }
    return line;
  });

  let fold: HTMLElement | null = null;
  let revealAdvanced = () => {};
  if (options.advanced) {
    const advanced = options.advanced;
    let open = advanced.open ?? false;
    const body = h("div", null, advanced.fields.map(build));
    body.style.cssText = advanced.columns
      ? `display:grid;grid-template-columns:${fitColumns(advanced.columns, 160, "var(--space-4)")};gap:var(--space-4)`
      : "display:flex;flex-direction:column;gap:var(--space-4)";
    // the chevron turns with `aria-expanded`, in the design system's CSS
    const disclosure = h(
      "button",
      { class: "bc-disclosure", type: "button" },
      icon(ICON_CHEVRON_RIGHT),
      s.advanced,
    );
    const paint = () => {
      disclosure.setAttribute("aria-expanded", String(open));
      body.hidden = !open;
      // the inline display (flex or grid) would otherwise override `hidden`
      body.style.display = open ? (advanced.columns ? "grid" : "flex") : "none";
    };
    disclosure.addEventListener("click", () => {
      open = !open;
      paint();
      advanced.onToggle?.(open);
    });
    paint();
    const label = h(
      "div",
      { class: "bc-row__label" },
      disclosure,
      h(
        "span",
        { class: "bc-small bc-muted" },
        pick(advanced.hint ?? s.advancedHint, lang),
      ),
    );
    fold = h(
      "div",
      { class: "bc-row bc-row--compact" },
      label,
      h("div", { class: "bc-row__body" }, body),
    );
    // an invalid default must be visible when submitting
    revealAdvanced = () => {
      if (open) return;
      open = true;
      paint();
      advanced.onToggle?.(open);
    };
  }

  const submit = h(
    "button",
    { class: "bc-button", type: "submit" },
    pick(options.submit, lang),
  );
  const footer = h(
    "div",
    { class: "bc-form__footer" },
    h(
      "button",
      { class: "bc-button bc-button--outline", type: "reset" },
      s.clear,
    ),
    submit,
  );
  const form = h(
    "form",
    { class: "bc-form", novalidate: true, autocomplete: "off" },
    rows,
    fold,
    footer,
  );

  const values = (): Values | null => {
    let first: Control | null = null;
    let hidden = false;
    const out: Values = {};
    for (const control of controls) {
      const message = control.validate();
      control.setError(message);
      if (message) {
        first ??= control;
        hidden ||= !!options.advanced?.fields.includes(control.field);
        continue;
      }
      const value = control.read();
      if (value !== undefined) out[control.field.name] = value;
    }
    if (first) {
      if (hidden) revealAdvanced();
      first.focus();
      return null;
    }
    return out;
  };

  let pending: Promise<SubmitOutcome> | null = null;
  const send = (): Promise<SubmitOutcome> => {
    if (pending) return pending;
    const current = values();
    if (!current) return Promise.resolve("rejected" as const);
    submit.disabled = true;
    form.setAttribute("aria-busy", "true");
    pending = submitJson(context, {
      endpoint: options.endpoint,
      method: "POST",
      body: options.body ? options.body(current) : current,
      success: options.success,
      onError: options.onError,
      retry: () => void send(),
    }).then((outcome) => {
      pending = null;
      if (!context.signal.aborted) {
        submit.disabled = false;
        form.removeAttribute("aria-busy");
      }
      if (outcome === "accepted") {
        for (const control of controls) control.clearSecret();
        options.onSuccess?.(current);
      }
      return outcome;
    });
    return pending;
  };

  form.addEventListener("submit", (event) => {
    event.preventDefault();
    void send();
  });
  form.addEventListener("reset", (event) => {
    event.preventDefault();
    for (const control of controls) control.reset();
  });
  context.signal.addEventListener(
    "abort",
    () => {
      for (const control of controls) control.clearSecret();
    },
    { once: true },
  );

  return {
    element: form,
    values,
    setError: (name, message, code) =>
      controls
        .find((control) => control.field.name === name)
        ?.setError(message, code),
    submit: send,
    footer,
  };
}
