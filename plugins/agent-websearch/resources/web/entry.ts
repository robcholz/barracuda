import {
  callDevice,
  definePage,
  entryStatus,
  h,
  header,
  kv,
  page,
  settingsForm,
} from "../../../captive-portal/resources/web/ui";

/** `GET /api/tavily/status`: `config` holds the active API base while configured, never the key. */
interface TavilyStatus {
  configured: boolean;
  config?: { api_base: string };
}

/**
 * The web search page: the Tavily key and API base for the `web_search` Tool (`POST /api/tavily`).
 * The header shows the API base the device uses (`GET /api/tavily/status`).
 */
export const mount = definePage((context) => {
  const status = entryStatus(context);
  const base = h("span", { class: "bc-mono" }, "—");
  const load = async () => {
    const result = await callDevice<TavilyStatus>(
      context,
      "/api/tavily/status",
    );
    if (result.kind === "ok" && typeof result.data?.configured === "boolean")
      base.textContent = result.data.config?.api_base ?? "—";
  };
  void load();
  const form = settingsForm(
    {
      endpoint: "/api/tavily",
      submit: { zh: "保存配置", en: "Save" },
      rows: [
        {
          title: "Tavily",
          hint: {
            zh: "在 Tavily 控制台创建 API Key",
            en: "Create an API key in the Tavily dashboard",
          },
          fields: [
            { kind: "secret", name: "api_key", label: "Tavily API Key" },
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
            value: "https://api.tavily.com",
          },
        ],
      },
      onSuccess: () => {
        void status.refresh();
        void load();
      },
      success: {
        action: {
          label: { zh: "去 Web 聊天试试", en: "Try it in Web chat" },
          run: () => context.navigate("imessage-web"),
        },
      },
    },
    context,
  );
  return page(
    header(
      {
        title: { zh: "网页搜索", en: "Web search" },
        lead: {
          zh: "让 Agent 通过 Tavily 搜索网页。",
          en: "Let the agent search the web through Tavily.",
        },
        extra: kv(
          [
            [{ zh: "状态", en: "Status" }, status.slot],
            ["API Base URL", base],
          ],
          context.lang,
          { live: true },
        ),
        figure: "agent-websearch",
        figureWidth: 280,
      },
      context.lang,
    ),
    form.element,
  );
});
