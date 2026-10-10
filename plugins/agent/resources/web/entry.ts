import {
  ICON_BOT,
  ICON_BRAIN,
  ICON_FOLD_VERTICAL,
  ICON_NETWORK,
  MARK_ANTHROPIC,
  MARK_OPENAI,
  callDevice,
  definePage,
  entryStatus,
  h,
  header,
  kv,
  page,
  settingsForm,
  type Text,
} from "../../../captive-portal/resources/web/ui";

type Purpose = "root_agent" | "sub_agent" | "memory" | "compaction";

/** One model in `GET /api/model-api/status`; the key is never reported. */
interface Model {
  backend: string;
  model: string;
  base_url: string;
}

/** `GET /api/model-api/status`: a purpose is `null` when it uses the default model. */
interface ModelsStatus {
  configured: boolean;
  default: Model | null;
  purposes: Record<Purpose, Model | null>;
}

const PURPOSES: readonly (readonly [Purpose, Text])[] = [
  ["root_agent", { zh: "主 Agent", en: "Main agent" }],
  ["sub_agent", { zh: "子 Agent", en: "Sub-agent" }],
  ["memory", { zh: "记忆", en: "Memory" }],
  ["compaction", { zh: "压缩", en: "Compaction" }],
];

/**
 * The model configuration page: one model per submission, for one Agent purpose
 * (`POST /api/model-api`). The header shows the model each purpose uses (`GET /api/model-api/status`).
 */
export const mount = definePage((context) => {
  const status = entryStatus(context);
  const models = PURPOSES.map(() => h("span", { class: "bc-mono" }, "—"));
  const load = async () => {
    const result = await callDevice<ModelsStatus>(
      context,
      "/api/model-api/status",
    );
    const data = result.kind === "ok" ? result.data : null;
    if (!data?.purposes) return;
    PURPOSES.forEach(([purpose], index) => {
      const model = data.purposes[purpose] ?? data.default;
      models[index]!.textContent = model?.model ?? "—";
    });
  };
  void load();
  const form = settingsForm(
    {
      endpoint: "/api/model-api",
      submit: { zh: "注册模型", en: "Register model" },
      rows: [
        {
          title: { zh: "接口协议", en: "API format" },
          hint: {
            zh: "服务商 API 的请求格式",
            en: "The request format of the provider's API",
          },
          fields: [
            {
              kind: "radio",
              name: "backend",
              label: { zh: "接口协议", en: "API format" },
              value: "openai_compatible",
              options: [
                {
                  value: "openai_compatible",
                  label: { zh: "OpenAI 兼容", en: "OpenAI-compatible" },
                  hint: {
                    zh: "Chat Completions，适用于多数服务商",
                    en: "Chat Completions; works with most providers",
                  },
                  icon: MARK_OPENAI,
                },
                {
                  value: "anthropic_compatible",
                  label: { zh: "Anthropic 兼容", en: "Anthropic-compatible" },
                  hint: { zh: "Messages 接口", en: "Messages API" },
                  icon: MARK_ANTHROPIC,
                },
              ],
            },
          ],
        },
        {
          title: { zh: "用途", en: "Purpose" },
          hint: {
            zh: "这个模型替哪类 Agent 工作",
            en: "Which agent this model works for",
          },
          fields: [
            {
              kind: "radio",
              name: "purpose",
              label: { zh: "用途", en: "Purpose" },
              value: "root_agent",
              options: [
                {
                  value: "root_agent",
                  label: { zh: "主 Agent", en: "Main agent" },
                  hint: { zh: "直接回复用户", en: "Replies to the user" },
                  icon: ICON_BOT,
                },
                {
                  value: "sub_agent",
                  label: { zh: "子 Agent", en: "Sub-agent" },
                  hint: { zh: "执行委派的任务", en: "Runs delegated tasks" },
                  icon: ICON_NETWORK,
                },
                {
                  value: "memory",
                  label: { zh: "记忆", en: "Memory" },
                  hint: {
                    zh: "整理长期记忆",
                    en: "Organizes long-term memory",
                  },
                  icon: ICON_BRAIN,
                },
                {
                  value: "compaction",
                  label: { zh: "压缩", en: "Compaction" },
                  hint: {
                    zh: "压缩对话上下文",
                    en: "Compacts the conversation context",
                  },
                  icon: ICON_FOLD_VERTICAL,
                },
              ],
            },
            {
              kind: "switch",
              name: "default",
              label: { zh: "设为默认模型", en: "Make default" },
              hint: {
                zh: "该用途优先使用这个模型",
                en: "Used first for this purpose",
              },
              value: true,
            },
          ],
        },
        {
          title: { zh: "连接", en: "Connection" },
          hint: {
            zh: "服务商提供的地址、模型 ID 与密钥",
            en: "The provider's address, model ID and key",
          },
          fields: [
            {
              kind: "url",
              name: "base_url",
              label: "API Base URL",
              placeholder: "https://api.example.com/v1",
            },
            {
              kind: "text",
              name: "model",
              label: { zh: "模型名称", en: "Model" },
              // an example ID: the field is mono, so its placeholder is a machine value too
              placeholder: "gpt-4o-mini",
            },
            { kind: "secret", name: "api_key", label: "API Key" },
          ],
        },
      ],
      advanced: {
        hint: { zh: "超时与大小上限", en: "Timeouts and size limits" },
        open: true,
        columns: 3,
        fields: [
          {
            kind: "number",
            name: "timeout_ms",
            label: { zh: "请求超时", en: "Request timeout" },
            value: 120000,
            unit: "ms",
          },
          {
            kind: "number",
            name: "max_tokens",
            label: { zh: "最大输出", en: "Max output" },
            value: 8192,
            unit: "tokens",
          },
          {
            kind: "number",
            name: "image_max_bytes",
            label: { zh: "图片上限", en: "Image limit" },
            value: 524288,
            unit: "bytes",
          },
        ],
      },
      // the endpoint takes a batch; this page registers one model at a time
      body: (values) => [values],
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
        title: { zh: "模型配置", en: "Models" },
        lead: {
          zh: "为每种 Agent 用途注册模型。",
          en: "Register a model for each agent purpose.",
        },
        extra: kv(
          [
            [{ zh: "状态", en: "Status" }, status.slot],
            ...PURPOSES.map(
              ([, label], index) => [label, models[index]!] as const,
            ),
          ],
          context.lang,
          { live: true },
        ),
        figure: "agent",
        figureWidth: 300,
      },
      context.lang,
    ),
    form.element,
  );
});
