import {
  ICON_MESSAGE_SQUARE_DASHED,
  ICON_PENCIL,
  ICON_PLUS,
  h,
  icon,
} from "../../../captive-portal/resources/web/ui";

/** Lucide `ellipsis`: a session's more-actions button. */
const ICON_ELLIPSIS =
  '<circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle><circle cx="5" cy="12" r="1"></circle>';
/** Lucide `trash-2`: delete. */
const ICON_TRASH =
  '<path d="M3 6h18"></path><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"></path><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"></path><line x1="10" x2="10" y1="11" y2="17"></line><line x1="14" x2="14" y1="11" y2="17"></line>';
/** Lucide `loader-circle`, spun by `.bc-spinner`. */
const ICON_SPINNER = '<path d="M21 12a9 9 0 1 1-6.219-8.56"></path>';

/** One of the conversation's saved sessions, as the device lists it. */
export interface SessionItem {
  session: string;
  title: string | null;
  updated_at: number | null;
  running: boolean;
}

/** The device's `conversation.sessions` event. */
export interface SessionsState {
  current: string | null;
  temporary: boolean;
  sessions: SessionItem[];
  notice: { kind: string; title?: string | null; temporary?: boolean } | null;
  now: number | null;
}

/** A session command for the device, sent as a control frame. */
export type SessionCommand =
  | { control: "sessions" }
  | { control: "new"; temporary?: boolean }
  | { control: "switch"; session: string }
  | { control: "rename"; session: string; title: string }
  | { control: "delete"; session: string; confirm: true };

export const SESSION_STRINGS = {
  zh: {
    sessions: "会话",
    newChat: "新会话",
    newTemp: "新临时会话",
    more: "更多操作",
    rename: "重命名",
    renameLabel: "会话名称",
    deleteSession: "删除会话",
    deleteTitle: "删除这个会话？",
    deleteBody: "「{title}」的记录会从设备上移除。",
    cancel: "取消",
    savedOnDevice: "保存在设备上",
    savedTip: "会话记录存放在设备闪存里",
    sessionsUnit: " 个会话",
    today: "今天",
    yesterday: "昨天",
    week: "过去 7 天",
    older: "更早",
    generating: "生成中",
    untitled: "新会话",
  },
  en: {
    sessions: "Sessions",
    newChat: "New session",
    newTemp: "New temporary chat",
    more: "More actions",
    rename: "Rename",
    renameLabel: "Session name",
    deleteSession: "Delete session",
    deleteTitle: "Delete this session?",
    deleteBody: "“{title}” is removed from the device.",
    cancel: "Cancel",
    savedOnDevice: "Saved on the device",
    savedTip: "Sessions are kept in the device's flash",
    sessionsUnit: " sessions",
    today: "Today",
    yesterday: "Yesterday",
    week: "Previous 7 days",
    older: "Older",
    generating: "Generating",
    untitled: "New session",
  },
};

const DAY = 86_400_000;

/** Which day group a last-used time falls in, by the reader's own calendar. */
function bucket(at: number | null, today: number) {
  if (at === null) return 3;
  if (at >= today) return 0;
  if (at >= today - DAY) return 1;
  if (at >= today - 6 * DAY) return 2;
  return 3;
}

/**
 * The session rail (`.bc-chat-rail`): 「新会话」 and the temporary-chat toggle, then the sessions
 * grouped by day, each with a more-actions menu (rename in place; delete asks first, in red), and
 * the count at its foot. It holds no state of its own: every change is a command to the device,
 * whose answer redraws it.
 */
export function sessionRail(
  lang: "zh" | "en",
  send: (command: SessionCommand) => void,
  signal: AbortSignal,
) {
  const t = SESSION_STRINGS[lang];
  let state: SessionsState | null = null;
  /** The session whose menu, rename field or delete confirmation is open. */
  let open: { session: string; mode: "menu" | "rename" | "confirm" } | null =
    null;

  const tempButton = h(
    "button",
    {
      class: "bc-button bc-button--outline bc-button--icon",
      type: "button",
      "aria-label": t.newTemp,
      "aria-pressed": "false",
      onclick: () => send({ control: "new", temporary: true }),
    },
    icon(ICON_MESSAGE_SQUARE_DASHED),
    h("span", { class: "bc-tooltip", "aria-hidden": "true" }, t.newTemp),
  );
  const nav = h("nav", {
    class: "bc-chat-rail__list",
    "aria-label": t.sessions,
  });
  const count = h("span", { class: "bc-mono" });
  const rail = h(
    "aside",
    { class: "bc-chat-rail", "aria-label": t.sessions },
    h(
      "div",
      { style: "display:flex;gap:8px" },
      h(
        "button",
        {
          class: "bc-button bc-button--outline",
          type: "button",
          style: "flex:1 1 auto",
          onclick: () => send({ control: "new" }),
        },
        icon(ICON_PLUS),
        t.newChat,
      ),
      tempButton,
    ),
    nav,
    h(
      "p",
      { class: "bc-caption bc-muted bc-chat-rail__foot" },
      h(
        "span",
        { class: "bc-term", tabindex: 0 },
        t.savedOnDevice,
        h(
          "span",
          { class: "bc-tooltip bc-tooltip--start", role: "tooltip" },
          t.savedTip,
        ),
      ),
      " · ",
      count,
      t.sessionsUnit,
    ),
  );

  const title = (item: SessionItem) => item.title ?? t.untitled;

  function close() {
    if (!open) return;
    open = null;
    render();
  }

  function entry(item: SessionItem) {
    const current = state?.current === item.session;
    const mode = open?.session === item.session ? open.mode : null;
    const more = h(
      "button",
      {
        class: "bc-icon-button bc-session__more",
        type: "button",
        "aria-label": t.more,
        "aria-haspopup": "menu",
        "aria-expanded": String(mode === "menu"),
        onclick: (event: Event) => {
          event.stopPropagation();
          open =
            mode === "menu" ? null : { session: item.session, mode: "menu" };
          render();
        },
      },
      icon(ICON_ELLIPSIS),
      h("span", { class: "bc-tooltip", "aria-hidden": "true" }, t.more),
    );
    let label: HTMLElement;
    if (mode === "rename") {
      const field = h("input", {
        class: "bc-input bc-session__rename",
        value: title(item),
        "aria-label": t.renameLabel,
        maxlength: 64,
      });
      let done = false;
      const finish = (save: boolean) => {
        if (done) return;
        done = true;
        const next = field.value.trim();
        if (save && next && next !== title(item))
          send({ control: "rename", session: item.session, title: next });
        close();
      };
      field.addEventListener("keydown", (event) => {
        if (event.key === "Enter" && !event.isComposing) finish(true);
        if (event.key === "Escape") finish(false);
      });
      field.addEventListener("blur", () => finish(true));
      queueMicrotask(() => {
        field.focus();
        field.select();
      });
      label = field;
    } else {
      label = h(
        "a",
        {
          class: "bc-nav-item",
          href: "#",
          "aria-current": current ? "page" : "false",
          onclick: (event: Event) => {
            event.preventDefault();
            rail.classList.remove("bc-chat-rail--open");
            if (!current) send({ control: "switch", session: item.session });
          },
        },
        h("span", { class: "bc-session__title" }, title(item)),
        item.running
          ? h(
              "span",
              { class: "bc-spinner", style: "display:inline-flex" },
              icon(ICON_SPINNER, undefined, "bc-muted"),
              h(
                "span",
                {
                  style:
                    "position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%)",
                },
                t.generating,
              ),
            )
          : null,
      );
    }
    const node = h(
      "div",
      { class: "bc-session bc-menu-anchor" },
      label,
      mode === "rename" ? null : more,
    );
    if (mode === "menu")
      node.append(
        h(
          "div",
          { class: "bc-menu", role: "menu" },
          h(
            "button",
            {
              class: "bc-menu-item",
              type: "button",
              role: "menuitem",
              onclick: (event: Event) => {
                event.stopPropagation();
                open = { session: item.session, mode: "rename" };
                render();
              },
            },
            icon(ICON_PENCIL),
            t.rename,
          ),
          h(
            "button",
            {
              class: "bc-menu-item bc-menu-item--destructive",
              type: "button",
              role: "menuitem",
              onclick: (event: Event) => {
                event.stopPropagation();
                open = { session: item.session, mode: "confirm" };
                render();
              },
            },
            icon(ICON_TRASH),
            t.deleteSession,
          ),
        ),
      );
    if (mode === "confirm") {
      const id = `imessage-web-delete-${item.session}`;
      node.append(
        h(
          "div",
          {
            class: "bc-menu bc-menu--confirm",
            role: "alertdialog",
            "aria-labelledby": `${id}-title`,
            "aria-describedby": `${id}-body`,
            onclick: (event: Event) => event.stopPropagation(),
          },
          h(
            "span",
            { style: "display:flex;flex-direction:column;gap:4px" },
            h(
              "span",
              { class: "bc-option-title", id: `${id}-title` },
              t.deleteTitle,
            ),
            h(
              "span",
              { class: "bc-small bc-muted", id: `${id}-body` },
              t.deleteBody.replace("{title}", title(item)),
            ),
          ),
          h(
            "span",
            { style: "display:flex;justify-content:flex-end;gap:8px" },
            h(
              "button",
              {
                class: "bc-button bc-button--outline bc-button--sm",
                type: "button",
                onclick: close,
              },
              t.cancel,
            ),
            h(
              "button",
              {
                class: "bc-button bc-button--danger bc-button--sm",
                type: "button",
                onclick: () => {
                  send({
                    control: "delete",
                    session: item.session,
                    confirm: true,
                  });
                  close();
                },
              },
              icon(ICON_TRASH),
              t.deleteSession,
            ),
          ),
        ),
      );
    }
    return node;
  }

  function render() {
    if (!state) return;
    tempButton.setAttribute("aria-pressed", String(state.temporary));
    count.textContent = String(state.sessions.length).padStart(2, "0");
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    const labels = [t.today, t.yesterday, t.week, t.older];
    const groups: SessionItem[][] = [[], [], [], []];
    for (const item of state.sessions)
      groups[bucket(item.updated_at, midnight.getTime())].push(item);
    nav.replaceChildren(
      ...groups.flatMap((items, index) =>
        items.length
          ? [
              h(
                "div",
                { class: "bc-nav-group" },
                h("span", { class: "bc-nav-label" }, labels[index]),
                items.map(entry),
              ),
            ]
          : [],
      ),
    );
  }

  // a click anywhere else closes an open menu or confirmation (their own clicks stop short)
  document.addEventListener(
    "click",
    () => {
      if (open && open.mode !== "rename") close();
    },
    { signal },
  );

  return {
    node: rail,
    /** Redraws from the device's latest answer. */
    update(next: SessionsState) {
      state = next;
      if (open && !next.sessions.some((item) => item.session === open!.session))
        open = null;
      render();
    },
    /** On a phone the rail is a sheet over the conversation. */
    toggle() {
      rail.classList.toggle("bc-chat-rail--open");
    },
  };
}
