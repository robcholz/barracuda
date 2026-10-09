import {
  button,
  channelInbound,
  configuredRow,
  definePage,
  h,
  header,
  note,
  page,
  qrLink,
  readChannel,
  resultCard,
  row,
  settingsForm,
  type ChannelStatus,
} from "../../../captive-portal/resources/web/ui";

const T = {
  zh: {
    lead: "通过 Telegram Bot 收发消息。",
    submit: "保存并替换通道",
    bot: "机器人",
    botHint: "在 `@BotFather` 发送 `/newbot` 获得 Token",
    botLink: "打开 @BotFather",
    verify: "验证",
    verified: "已验证",
    invalid: "检查 Bot Token 后重新验证。",
    unreachable: "连不上 Telegram，Token 未验证",
    chat: "开始对话",
    chatHint: "用手机扫码，打开和 Bot 的对话",
    open: "在 Telegram 中打开",
    draft: "草稿最小增量",
    tryChat: "去 Web 聊天试试",
    how: "在 Telegram 里给 Bot 发送",
  },
  en: {
    lead: "Send and receive messages through a Telegram bot.",
    submit: "Save and replace channel",
    bot: "Bot",
    botHint: "Send `/newbot` to `@BotFather` to get a token",
    botLink: "Open @BotFather",
    verify: "Verify",
    verified: "Verified",
    invalid: "Check the bot token and verify again.",
    unreachable: "Couldn't reach Telegram; the token is not verified",
    chat: "Start a chat",
    chatHint: "Scan with your phone to open a chat with the bot",
    open: "Open in Telegram",
    draft: "Draft minimum delta",
    tryChat: "Try it in Web chat",
    how: "In Telegram, send the bot",
  },
};

/** The parts of Bot API `getMe` the page reads. */
interface GetMe {
  ok?: boolean;
  result?: { first_name?: string; username?: string };
  error_code?: number;
  description?: string;
}

const show = (node: HTMLElement, visible: boolean) => (node.hidden = !visible);

const ENDPOINT = "/api/gateway/telegram";

/** `GET /api/gateway/telegram/status`: `config` holds the bot's numeric ID while configured. */
type TelegramStatus = ChannelStatus & { config?: { bot_id: string | null } };

/**
 * The Telegram page: the bot token, checked in the browser with Bot API `getMe` (Telegram allows any
 * origin), then saved with `POST /api/gateway/telegram`. `GET /status` below it says whether a
 * channel is configured and for which bot; the page then shows it above the form that replaces
 * it, with the channel's mode and allowed accounts (`/mode`, `/owners`) before the 「高级」 fold.
 */
export const mount = definePage((context) => {
  const { lang } = context;
  const t = T[lang];
  const result = h("div", { style: { display: "contents" } });
  const verify = button(t.verify, lang, {
    variant: "outline",
    onClick: () => void check(),
  });
  const current = configuredRow("Telegram", lang);
  const showCurrent = (state: TelegramStatus | null) => {
    if (state?.configured)
      current.show(true, state.config?.bot_id ?? undefined);
  };
  const inbound = channelInbound(context, {
    endpoint: ENDPOINT,
    channel: "Telegram",
    how: t.how,
    command: (code) => `/start ${code}`,
  });
  const form = settingsForm(
    {
      endpoint: ENDPOINT,
      submit: t.submit,
      rows: [
        {
          title: t.bot,
          hint: t.botHint,
          link: { label: t.botLink, href: "https://t.me/BotFather" },
          fields: [
            {
              kind: "secret",
              name: "token",
              label: "Bot Token",
              action: verify,
            },
          ],
          blocks: result,
        },
      ],
      advanced: {
        open: true,
        fields: [
          {
            kind: "url",
            name: "api_base",
            label: "API Base URL",
            value: "https://api.telegram.org",
          },
          {
            kind: "number",
            name: "draft_min_delta_bytes",
            label: t.draft,
            value: 24,
            unit: "bytes",
          },
        ],
      },
      onError: (error) => ({ body: error.message }),
      onSuccess: () => {
        current.show(true);
        void inbound.refresh().then(showCurrent);
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
  form.element.prepend(current.element);
  inbound.attach(form);
  void readChannel<TelegramStatus>(context, ENDPOINT).then((state) => {
    showCurrent(state);
    inbound.apply(state);
  });
  const token = form.element.querySelector<HTMLInputElement>('[name="token"]')!;
  const chat = row(t.chat, t.chatHint, lang);
  show(chat, false);
  token.closest(".bc-row")!.after(chat);

  let run = 0;
  const clear = () => {
    run++;
    result.replaceChildren();
    chat.lastElementChild!.replaceChildren();
    show(chat, false);
  };
  token.addEventListener("input", clear);
  form.element.addEventListener("reset", clear);

  async function check() {
    const values = form.values();
    if (!values) return;
    clear();
    const mine = run;
    const base = String(values.api_base).replace(/\/+$/, "");
    const request = new AbortController();
    const cancel = () => request.abort();
    context.signal.addEventListener("abort", cancel, { once: true });
    const timeout = setTimeout(cancel, 10_000);
    verify.disabled = true;
    try {
      const response = await fetch(
        `${base}/bot${String(values.token).trim()}/getMe`,
        { signal: request.signal, cache: "no-store", credentials: "omit" },
      );
      const reply = (await response.json()) as GetMe;
      if (mine !== run || context.signal.aborted) return;
      const bot = reply.result;
      if (reply.ok && bot?.username) {
        const name = bot.first_name || bot.username;
        result.replaceChildren(
          resultCard(
            {
              title: name,
              badge: t.verified,
              sub: `@${bot.username}`,
              initial: [...name][0]?.toUpperCase(),
            },
            lang,
          ),
        );
        chat.lastElementChild!.replaceChildren(
          qrLink(`https://t.me/${bot.username}`, t.open, lang),
        );
        show(chat, true);
      } else {
        const code = [reply.error_code ?? response.status, reply.description]
          .filter(Boolean)
          .join(" ");
        form.setError("token", t.invalid, code);
      }
    } catch {
      // no route out (the phone is on the device's hotspot) or not the Bot API: saving still works
      if (mine === run && !context.signal.aborted)
        result.replaceChildren(note(t.unreachable, lang));
    } finally {
      clearTimeout(timeout);
      context.signal.removeEventListener("abort", cancel);
      if (!context.signal.aborted) verify.disabled = false;
    }
  }

  return page(
    header(
      {
        title: "Telegram",
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
