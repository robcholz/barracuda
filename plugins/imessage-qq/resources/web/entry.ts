import {
  ICON_COPY,
  ICON_REFRESH,
  button,
  callDevice,
  channelInbound,
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
    lead: "用手机 QQ 扫码，把设备接入 QQ 消息通道。",
    submit: "验证并保存",
    advancedHint: "手动填写 App ID 和 App Secret",
    scan: "扫码绑定",
    scanHint: "用手机 QQ 扫码，选好机器人并确认",
    steps: ["用 QQ 扫描二维码", "在手机上选好机器人", "绑定完成"],
    renew: "换一张二维码",
    expired: "二维码已过期",
    failed: "绑定失败",
    failedHint: "检查设备的网络后，换一张二维码再试。",
    link: "绑定",
    linkHint: "这台设备在 QQ 里的机器人",
    linked: "QQ 已绑定",
    relink: "重新绑定",
    elsewhere: "在电脑或另一台设备上打开此页扫码",
    copy: "复制链接",
    copied: "已复制链接",
    copyFailed: "复制失败",
    rejected: "检查 App ID 和 App Secret。",
    tryChat: "去 Web 聊天试试",
    how: "在 QQ 里给机器人发送",
  },
  en: {
    lead: "Scan with QQ to connect the device to the QQ channel.",
    submit: "Verify and save",
    advancedHint: "Enter App ID and App Secret by hand",
    scan: "Link by QR",
    scanHint: "Scan with QQ on your phone, pick a bot and confirm",
    steps: ["Scan the code in QQ", "Pick a bot on your phone", "Linked"],
    renew: "New code",
    expired: "Code expired",
    failed: "Linking failed",
    failedHint: "Check the device's network, then get a new QR code.",
    link: "Link",
    linkHint: "This device as a bot in QQ",
    linked: "QQ linked",
    relink: "Link again",
    elsewhere: "Open this page on another device to scan",
    copy: "Copy link",
    copied: "Link copied",
    copyFailed: "Couldn't copy the link",
    rejected: "Check the App ID and App Secret.",
    tryChat: "Try it in Web chat",
    how: "In QQ, send the bot",
  },
};

const ENDPOINT = "/api/gateway/qq";
const LOGIN = "/api/gateway/qq/login";
/** The shell's phone breakpoint: no one scans a code on the screen of the phone that scans it. */
const PHONE = "(max-width: 719px)";
const POLL_MS = 2_000;

type Status = "idle" | "wait" | "confirmed" | "expired" | "failed";
interface LoginState {
  status?: Status;
  configured?: boolean;
  app_id?: string;
  message?: string;
}

type View =
  | { kind: "loading" }
  | { kind: "wait"; url: string }
  | { kind: "ended"; url: string | null; label: Text }
  | { kind: "linked"; appId: string | null }
  | { kind: "phone" };

const show = (node: HTMLElement, visible: boolean) => (node.hidden = !visible);

/**
 * The QQ page. The device runs one scan-to-bind session (`POST/GET/DELETE /api/gateway/qq/login`):
 * the page starts it, shows the code, polls every 2 s and cancels it when it goes away. QQ reports
 * no scan, only the finished binding or an expired code. An App ID and App Secret entered by hand
 * under 「高级」 post to `POST /api/gateway/qq`, which checks them with QQ first; QQ's rejection
 * (422 `verification_failed`) is shown on the secret field. Once linked, `GET /status` below it shows
 * the channel's mode and allowed accounts.
 */
export const mount = definePage((context) => {
  const { lang } = context;
  const t = T[lang];
  const media = window.matchMedia(PHONE);

  const form = settingsForm(
    {
      endpoint: ENDPOINT,
      submit: t.submit,
      rows: [],
      advanced: {
        hint: t.advancedHint,
        onToggle: (open) => show(form.footer, open),
        fields: [
          { kind: "text", name: "app_id", label: "App ID" },
          { kind: "secret", name: "app_secret", label: "App Secret" },
          {
            kind: "url",
            name: "api_base",
            label: "API Base URL",
            value: "https://api.sgroup.qq.com",
          },
          {
            kind: "url",
            name: "token_url",
            label: "Token URL",
            value: "https://bots.qq.com/app/getAppAccessToken",
          },
        ],
      },
      // the field says what to do; QQ's own words go in the toast
      onError: (error) => {
        if (error.error === "verification_failed")
          form.setError("app_secret", t.rejected, error.code);
        return { body: error.message };
      },
      onSuccess: (values) => {
        end();
        view = { kind: "linked", appId: String(values.app_id) };
        render();
        void context.refreshStatus();
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
  const inbound = channelInbound(context, {
    endpoint: ENDPOINT,
    channel: "QQ",
    how: t.how,
  });
  inbound.attach(form);
  /** The mode and accounts rows show the device's channel while the page shows it linked. */
  let linkedShown = false;

  let view: View = { kind: "loading" };
  let slot: HTMLElement = h("div");
  form.element.prepend(slot);
  /** This page started a device session that may still be running. */
  let session = false;
  let poll: ReturnType<typeof setTimeout> | undefined;
  let attempt = 0;

  /** Stops polling and cancels the device's session, if this page has one running. */
  const end = () => {
    clearTimeout(poll);
    poll = undefined;
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

  function render() {
    let next: HTMLElement;
    if (view.kind === "linked") {
      const relink = button(t.relink, lang, {
        variant: "outline",
        size: "sm",
        onClick: () => void begin(),
      });
      next = row(
        t.link,
        t.linkHint,
        lang,
        resultCard(
          {
            title: t.linked,
            rows: view.appId ? [["App ID", view.appId]] : undefined,
            action: relink,
          },
          lang,
        ),
      );
    } else {
      const steps = stepList(
        t.steps,
        view.kind === "ended"
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
        plate = qrPlate(url, 168, lang, {
          dim: view.kind === "ended" ? view.label : undefined,
        });
        const renew = button(t.renew, lang, {
          variant: view.kind === "ended" ? "primary" : "outline",
          icon: ICON_REFRESH,
          onClick: () => void begin(),
        });
        renew.disabled = view.kind === "loading";
        side.append(h("div", null, renew));
      }
      // a Card whose body holds the QR plate beside its Steps
      const body = h("div", { class: "bc-card__body" }, plate, side);
      body.style.cssText =
        "display:flex;flex-wrap:wrap;gap:24px;align-items:center";
      next = row(
        t.scan,
        t.scanHint,
        lang,
        h("div", { class: "bc-card" }, body),
      );
    }
    slot.replaceWith(next);
    slot = next;
    if ((view.kind === "linked") === linkedShown) return;
    linkedShown = view.kind === "linked";
    if (linkedShown) void inbound.refresh();
    else inbound.apply(null);
  }

  const finish = (next: View) => {
    clearTimeout(poll);
    poll = undefined;
    view = next;
    render();
  };

  /** Starts a fresh binding on the device (cancelling any other) and shows its code. */
  async function begin() {
    if (media.matches) {
      end();
      return finish({ kind: "phone" });
    }
    // no DELETE first: the POST replaces any session, and a late DELETE would cancel the new one
    const mine = ++attempt;
    finish({ kind: "loading" });
    session = true;
    const result = await callDevice<{ url?: string }>(context, LOGIN, {
      method: "POST",
      body: {},
    });
    if (result.kind === "aborted" || mine !== attempt) return;
    if (result.kind !== "ok" || !result.data?.url) {
      session = false;
      finish({ kind: "ended", url: null, label: t.failed });
      if (result.kind !== "ok") toastDeviceError(context, result);
      return;
    }
    finish({ kind: "wait", url: result.data.url });
    poll = setTimeout(check, POLL_MS);
  }

  async function check() {
    const mine = attempt;
    const result = await callDevice<LoginState>(context, LOGIN);
    if (result.kind === "aborted" || mine !== attempt) return;
    const url = "url" in view ? view.url : null;
    const state = result.kind === "ok" ? result.data : null;
    switch (state?.status) {
      case "confirmed":
        session = false;
        finish({ kind: "linked", appId: state.app_id ?? null });
        context.toast({ kind: "success", title: t.linked });
        void context.refreshStatus();
        return;
      case "expired":
      case "idle":
        // idle: the session was cancelled elsewhere (another page started its own)
        session = false;
        return finish({ kind: "ended", url, label: t.expired });
      case "failed": {
        session = false;
        finish({ kind: "ended", url, label: t.failed });
        // the device's message names its internals (a TLS error, say), so the toast says what to do
        context.toast({ kind: "error", title: t.failed, body: t.failedHint });
        return;
      }
      // still waiting, no reply or an unexpected one: keep asking until the code expires
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
    const login = state.kind === "ok" ? state.data : null;
    if (login?.configured && login.status !== "wait")
      finish({ kind: "linked", appId: login.app_id ?? null });
    else void begin();
  })();

  return page(
    header(
      {
        title: "QQ",
        lead: t.lead,
        icon: new URL("./icon.svg", import.meta.url).href,
        figure: "riffle",
        figureWidth: 280,
      },
      lang,
    ),
    inbound.alert,
    form.element,
  );
});
