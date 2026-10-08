import {
  ICON_COPY,
  ICON_REFRESH,
  button,
  callDevice,
  copyText,
  definePage,
  h,
  header,
  page,
  qrPlate,
  resultCard,
  row,
  settingsForm,
  stepList,
  toastDeviceError,
  type Text,
} from "../../../captive-portal/resources/web/ui";

const T = {
  zh: {
    title: "微信",
    lead: "用微信扫码，把设备接入微信消息通道。",
    submit: "用 Token 保存",
    advancedHint: "手动填写 Token",
    scan: "扫码绑定",
    scanHint: "用手机微信扫码并确认",
    steps: ["用微信扫描二维码", "在手机上确认", "绑定完成"],
    expiresIn: "二维码过期还剩",
    renew: "换一张二维码",
    scanned: "已扫码",
    expired: "二维码已过期",
    failed: "绑定失败",
    link: "绑定",
    linkHint: "这台设备在微信里的 ClawBot",
    linked: "微信已绑定",
    linkedBadge: "已绑定",
    relink: "重新绑定",
    elsewhere: "在电脑或另一台设备上打开此页扫码",
    copy: "复制链接",
    copied: "已复制链接",
    copyFailed: "复制失败",
    clientVersion: "客户端版本",
    routeTag: "路由标签",
    tryChat: "去 Web 聊天试试",
  },
  en: {
    title: "WeChat",
    lead: "Scan with WeChat to connect the device to the WeChat channel.",
    submit: "Save with token",
    advancedHint: "Enter a token by hand",
    scan: "Link by QR",
    scanHint: "Scan with WeChat on your phone, then confirm",
    steps: ["Scan the code in WeChat", "Confirm on your phone", "Linked"],
    expiresIn: "Code expires in",
    renew: "New code",
    scanned: "Scanned",
    expired: "Code expired",
    failed: "Linking failed",
    link: "Link",
    linkHint: "This device as a ClawBot in WeChat",
    linked: "WeChat linked",
    linkedBadge: "Linked",
    relink: "Link again",
    elsewhere: "Open this page on another device to scan",
    copy: "Copy link",
    copied: "Link copied",
    copyFailed: "Couldn't copy the link",
    clientVersion: "Client version",
    routeTag: "Route tag",
    tryChat: "Try it in Web chat",
  },
};

const LOGIN = "/api/gateway/wechat/login";
/** The shell's phone breakpoint: no one scans a code on the screen of the phone that scans it. */
const PHONE = "(max-width: 719px)";
const POLL_MS = 2_000;

type Status = "idle" | "wait" | "scanned" | "confirmed" | "expired" | "failed";
interface LoginState {
  status?: Status;
  configured?: boolean;
  message?: string;
}

type View =
  | { kind: "loading" }
  | { kind: "wait" | "scanned"; url: string }
  | { kind: "ended"; url: string | null; label: Text }
  | { kind: "linked" }
  | { kind: "phone" };

const show = (node: HTMLElement, visible: boolean) =>
  (node.style.display = visible ? "" : "none");

/**
 * The WeChat page. The device runs one iLink QR login (`POST/GET/DELETE /api/gateway/wechat/login`):
 * the page starts it, shows the code, polls every 2 s and cancels it when it goes away. A token
 * entered by hand under 「高级」 posts to `POST /api/gateway/wechat`.
 */
export const mount = definePage((context) => {
  const { lang } = context;
  const t = T[lang];
  const media = window.matchMedia(PHONE);

  const form = settingsForm(
    {
      endpoint: "/api/gateway/wechat",
      submit: t.submit,
      rows: [],
      advanced: {
        hint: t.advancedHint,
        onToggle: (open) => show(form.footer, open),
        fields: [
          { kind: "secret", name: "token", label: "Token" },
          {
            kind: "url",
            name: "api_base",
            label: "API Base URL",
            value: "https://ilinkai.weixin.qq.com",
          },
          { kind: "text", name: "app_id", label: "App ID", value: "bot" },
          {
            kind: "text",
            name: "client_version",
            label: t.clientVersion,
            value: "131329",
          },
          {
            kind: "text",
            name: "x_wechat_uin",
            label: "X-Wechat-UIN",
            value: "MA==",
          },
          {
            kind: "text",
            name: "route_tag",
            label: t.routeTag,
            optional: true,
          },
        ],
      },
      onError: (error) => ({ body: error.message }),
      onSuccess: () => {
        end();
        view = { kind: "linked" };
        render();
      },
      success: {
        action: {
          label: t.tryChat,
          run: () => context.navigate("imessage-web"),
        },
      },
    },
    context,
  );
  show(form.footer, false);

  let view: View = { kind: "loading" };
  let slot: HTMLElement = h("div");
  form.element.prepend(slot);
  /** This page started a device session that may still be running. */
  let session = false;
  let deadline = 0;
  let poll: ReturnType<typeof setTimeout> | undefined;
  let tick: ReturnType<typeof setInterval> | undefined;
  let countdown: HTMLElement | null = null;
  let attempt = 0;

  const stop = () => {
    clearTimeout(poll);
    clearInterval(tick);
    poll = tick = undefined;
  };
  /** Stops polling and cancels the device's session, if this page has one running. */
  const end = () => {
    stop();
    attempt++;
    if (!session) return;
    session = false;
    // keepalive: the request must outlive a page that is closing
    void fetch(LOGIN, {
      method: "DELETE",
      keepalive: true,
      cache: "no-store",
      redirect: "error",
    }).catch(() => {});
  };

  const remaining = () => {
    const seconds = Math.max(0, Math.ceil((deadline - Date.now()) / 1000));
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
  };

  function render() {
    countdown = null;
    let next: HTMLElement;
    if (view.kind === "linked") {
      next = row(
        t.link,
        t.linkHint,
        lang,
        resultCard(
          {
            title: t.linked,
            badge: t.linkedBadge,
            action: button(t.relink, lang, {
              variant: "outline",
              size: "sm",
              onClick: () => void begin(),
            }),
          },
          lang,
        ),
      );
    } else {
      const steps = stepList(
        t.steps,
        view.kind === "scanned"
          ? { current: 1, done: 1 }
          : view.kind === "ended"
            ? { current: -1, done: 0 }
            : { current: 0, done: 0 },
        lang,
      );
      const side = h("div", null, steps);
      side.style.cssText =
        "flex:1 1 220px;display:flex;flex-direction:column;gap:16px";
      let plate: HTMLElement | null = null;
      if (view.kind === "phone") {
        const line = h("p", { class: "bc-small bc-muted" }, t.elsewhere);
        line.style.margin = "0";
        side.append(
          line,
          h(
            "div",
            null,
            button(t.copy, lang, {
              variant: "outline",
              icon: ICON_COPY,
              onClick: () => void copyLink(),
            }),
          ),
        );
      } else {
        const url = view.kind === "loading" ? null : view.url;
        const dim =
          view.kind === "scanned"
            ? t.scanned
            : view.kind === "ended"
              ? view.label
              : undefined;
        plate = qrPlate(url, 168, lang, { dim });
        if (view.kind === "wait" || view.kind === "scanned") {
          countdown = h("span", { class: "bc-mono" }, remaining());
          side.append(
            h(
              "span",
              { class: "bc-small bc-muted" },
              `${t.expiresIn} `,
              countdown,
            ),
          );
        }
        const renew = button(t.renew, lang, {
          variant: view.kind === "ended" ? "primary" : "outline",
          icon: ICON_REFRESH,
          onClick: () => void begin(),
        });
        renew.disabled = view.kind === "loading";
        side.append(h("div", null, renew));
      }
      const card = h("div", { class: "bc-frame" }, plate, side);
      card.style.cssText =
        "display:flex;flex-wrap:wrap;gap:24px;align-items:center;padding:20px";
      next = row(t.scan, t.scanHint, lang, card);
    }
    slot.replaceWith(next);
    slot = next;
  }

  const finish = (next: View) => {
    stop();
    view = next;
    render();
  };

  /** Starts a fresh login on the device (cancelling any other) and shows its code. */
  async function begin() {
    if (media.matches) {
      end();
      return finish({ kind: "phone" });
    }
    // no DELETE first: the POST replaces any session, and a late DELETE would cancel the new one
    stop();
    const mine = ++attempt;
    finish({ kind: "loading" });
    session = true;
    const result = await callDevice<{ url?: string; expires_in?: number }>(
      context,
      LOGIN,
      { method: "POST", body: {} },
    );
    if (result.kind === "aborted" || mine !== attempt) return;
    if (result.kind !== "ok" || !result.data?.url) {
      session = false;
      finish({ kind: "ended", url: null, label: t.failed });
      if (result.kind !== "ok") toastDeviceError(context, result);
      return;
    }
    deadline = Date.now() + (result.data.expires_in ?? 480) * 1000;
    finish({ kind: "wait", url: result.data.url });
    tick = setInterval(() => {
      if (Date.now() >= deadline && view.kind !== "ended" && "url" in view) {
        attempt++; // a poll still in flight must not bring the code back
        finish({ kind: "ended", url: view.url, label: t.expired });
        return;
      }
      if (countdown) countdown.textContent = remaining();
    }, 1000);
    poll = setTimeout(check, POLL_MS);
  }

  async function check() {
    const mine = attempt;
    const result = await callDevice<LoginState>(context, LOGIN);
    if (result.kind === "aborted" || mine !== attempt) return;
    const url = "url" in view ? view.url : null;
    switch (result.kind === "ok" ? result.data?.status : undefined) {
      case "wait":
        if (url && view.kind !== "wait") finish({ kind: "wait", url });
        break;
      case "scanned":
        if (url && view.kind !== "scanned") finish({ kind: "scanned", url });
        break;
      case "confirmed":
        session = false;
        finish({ kind: "linked" });
        context.toast({ kind: "success", title: t.linked });
        return;
      case "expired":
      case "idle":
        // idle: the session was cancelled elsewhere (another page started its own)
        session = false;
        return finish({ kind: "ended", url, label: t.expired });
      case "failed": {
        session = false;
        finish({ kind: "ended", url, label: t.failed });
        const message = result.kind === "ok" ? result.data?.message : undefined;
        context.toast({ kind: "error", title: t.failed, body: message });
        return;
      }
      // no reply or an unexpected one: keep asking until the code expires
    }
    poll = setTimeout(check, POLL_MS);
  }

  async function copyLink() {
    const done = await copyText(window.location.href);
    if (context.signal.aborted) return;
    context.toast(
      done
        ? { kind: "success", title: t.copied }
        : { kind: "error", title: t.copyFailed, body: window.location.href },
    );
  }

  const resize = () => {
    if (media.matches === (view.kind === "phone")) return;
    if (view.kind === "linked") return;
    void begin();
  };
  media.addEventListener("change", resize);
  context.signal.addEventListener(
    "abort",
    () => {
      media.removeEventListener("change", resize);
      end();
    },
    { once: true },
  );

  render();
  void (async () => {
    const mine = attempt;
    const state = await callDevice<LoginState>(context, LOGIN);
    if (state.kind === "aborted" || mine !== attempt) return;
    const status = state.kind === "ok" ? state.data?.status : undefined;
    const running = status === "wait" || status === "scanned";
    if (state.kind === "ok" && state.data?.configured && !running)
      finish({ kind: "linked" });
    else void begin();
  })();

  return page(
    header(
      {
        title: t.title,
        lead: t.lead,
        icon: new URL("./icon.svg", import.meta.url).href,
        figure: "riffle",
        figureWidth: 280,
      },
      lang,
    ),
    form.element,
  );
});
