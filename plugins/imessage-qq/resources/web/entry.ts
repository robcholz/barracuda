import {
  KIT_STRINGS,
  definePage,
  header,
  page,
  settingsForm,
} from "../../../captive-portal/resources/web/ui";

const T = {
  zh: {
    lead: "通过 QQ Bot 收发消息。",
    submit: "验证并保存",
    bot: "机器人",
    botHint: "在 QQ 开放平台创建机器人，从「开发设置」复制",
    botLink: "打开 QQ 开放平台",
    platform: "QQ 开放平台：",
    tryChat: "去 Web 聊天试试",
  },
  en: {
    lead: "Send and receive messages through a QQ bot.",
    submit: "Verify and save",
    bot: "Bot",
    botHint:
      "Create a bot on the QQ Open Platform and copy these from Development settings",
    botLink: "Open the QQ Open Platform",
    platform: "QQ Open Platform: ",
    tryChat: "Try it in Web chat",
  },
};

/**
 * The QQ page: App ID and App Secret, posted to `POST /api/gateway/qq`. The device fetches one access
 * token before it stores anything; QQ's own rejection (422 `verification_failed`) is shown on the
 * secret field.
 */
export const mount = definePage((context) => {
  const { lang } = context;
  const t = T[lang];
  const form = settingsForm(
    {
      endpoint: "/api/gateway/qq",
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
      onError: (error) => {
        if (error.error === "verification_failed")
          form.setError(
            "app_secret",
            `${t.platform}${error.message ?? KIT_STRINGS[lang].rejected}`,
            error.code,
          );
        return { body: error.message };
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
    form.element,
  );
});
