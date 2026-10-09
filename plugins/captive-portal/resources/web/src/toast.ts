import type { Toast } from "./contract";
import { h, icon } from "../ui/dom";
import {
  ICON_CIRCLE_ALERT,
  ICON_CIRCLE_CHECK,
  ICON_INFO,
  ICON_X,
} from "../ui/icons";

/** How long a success or info toast stays; errors stay until closed. */
export const TOAST_MS = 4000;
const LIMIT = 4;

/** The shell's toast stack, bottom-right (the design's Toast; `bc-toaster`). */
export class Toaster {
  readonly element: HTMLElement;
  private readonly timers = new Map<
    HTMLElement,
    ReturnType<typeof setTimeout>
  >();

  constructor(
    private closeLabel: () => string,
    private duration = TOAST_MS,
  ) {
    this.element = h("div", { class: "bc-toaster" });
  }

  show(toast: Toast): HTMLElement {
    const error = toast.kind === "error";
    const glyph = icon(
      error
        ? ICON_CIRCLE_ALERT
        : toast.kind === "success"
          ? ICON_CIRCLE_CHECK
          : ICON_INFO,
      undefined,
      "bc-toast__icon",
    );
    // the Toast card's markup: the icon, then a body holding the title, the line and the action
    const node: HTMLElement = h(
      "div",
      {
        class: `bc-toast${error ? " bc-toast--error" : toast.kind === "success" ? " bc-toast--success" : ""}`,
        role: error ? "alert" : "status",
      },
      glyph,
      h(
        "div",
        { class: "bc-toast__body" },
        h(
          "span",
          { class: "bc-toast__title" },
          toast.title,
          toast.code
            ? [
                " ",
                h("span", { class: "bc-mono bc-caption bc-muted" }, toast.code),
              ]
            : null,
        ),
        toast.body
          ? h("span", { class: "bc-small bc-muted" }, toast.body)
          : null,
        toast.action
          ? h(
              "button",
              {
                class: "bc-button bc-button--outline bc-button--sm",
                type: "button",
                onclick: () => {
                  this.dismiss(node);
                  toast.action?.run();
                },
              },
              toast.action.label,
            )
          : null,
      ),
      h(
        "button",
        {
          class: "bc-toast__close",
          type: "button",
          "aria-label": this.closeLabel(),
          onclick: () => this.dismiss(node),
        },
        icon(ICON_X),
      ),
    );
    this.element.append(node);
    if (!error)
      this.timers.set(
        node,
        setTimeout(() => this.dismiss(node), this.duration),
      );
    // the oldest toasts give way first, so the stack never covers the page
    while (this.element.childElementCount > LIMIT)
      this.dismiss(this.element.firstElementChild as HTMLElement);
    return node;
  }

  dismiss(node: HTMLElement) {
    clearTimeout(this.timers.get(node));
    this.timers.delete(node);
    node.remove();
  }

  clear() {
    for (const node of [...this.element.children] as HTMLElement[])
      this.dismiss(node);
  }
}
