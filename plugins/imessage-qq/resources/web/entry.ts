import {
  channelInbound,
  configuredRow,
  definePage,
  header,
  page,
  readChannel,
  settingsForm,
  type ChannelStatus,
} from "../../../captive-portal/resources/web/ui";

const T = {
  zh: {
    lead: "通过 QQ Bot 收发消息。",
    submit: "验证并保存",
    bot: "机器人",
    botHint: "在 QQ 开放平台创建机器人，从「开发设置」复制",
    botLink: "打开 QQ 开放平台",
    rejected: "检查 App ID 和 App Secret。",
    tryChat: "去 Web 聊天试试",
    how: "在 QQ 里给机器人发送",
  },
  en: {
    lead: "Send and receive messages through a QQ bot.",
    submit: "Verify and save",
    bot: "Bot",
    botHint:
      "Create a bot on the QQ Open Platform and copy these from Development settings",
    botLink: "Open the QQ Open Platform",
    rejected: "Check the App ID and App Secret.",
    tryChat: "Try it in Web chat",
    how: "In QQ, send the bot",
  },
};

const ENDPOINT = "/api/gateway/qq";

/**
 * The QQ page: App ID and App Secret, posted to `POST /api/gateway/qq`. The device fetches one access
 * token before it stores anything; QQ's own rejection (422 `verification_failed`) is shown on the
 * secret field. `GET /status` below it says whether a channel is configured, and its mode and
 * allowed accounts (`/mode`, `/owners`) show while it is.
 */
export const mount = definePage((context) => {
  const { lang } = context;
  const t = T[lang];
  const current = configuredRow("QQ", lang);
  const inbound = channelInbound(context, {
    endpoint: ENDPOINT,
    channel: "QQ",
    how: t.how,
  });
  const form = settingsForm(
    {
      endpoint: ENDPOINT,
      submit: t.submit,
      rows: [
        {
          title: t.bot,
          hint: t.botHint,
          link: { label: t.botLink, href: "https://q.qq.com" },
          fields: [
            { kind: "text", name: "app_id", label: "App ID" },
            { kind: "secret", name: "app_secret", label: "App Secret" },
          ],
        },
      ],
      advanced: {
        open: true,
        fields: [
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
      onSuccess: () => {
        current.show(true);
        void inbound.refresh();
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
  void readChannel<ChannelStatus>(context, ENDPOINT).then((state) => {
    if (state?.configured) current.show(true);
    inbound.apply(state);
  });
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
