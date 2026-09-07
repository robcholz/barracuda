import { configuration } from "../../../captive-portal/resources/web/ui/form";

export const mount = configuration({
  title: "模型配置",
  endpoint: "/api/model-api",
  description: "为指定 Agent 用途注册模型。每次提交一个模型配置。",
  fields: [
    {
      name: "backend",
      label: "接口协议",
      options: ["openai_compatible", "anthropic_compatible"],
    },
    {
      name: "purpose",
      label: "用途",
      options: ["root_agent", "sub_agent", "memory", "compaction"],
    },
    {
      name: "default",
      label: "设为该用途默认模型",
      type: "checkbox",
      value: true,
    },
    {
      name: "base_url",
      label: "API Base URL",
      type: "url",
    },
    {
      name: "model",
      label: "模型名称",
    },
    {
      name: "api_key",
      label: "API Key",
      type: "password",
    },
    {
      name: "timeout_ms",
      label: "请求超时（毫秒）",
      type: "number",
      value: 120000,
    },
    {
      name: "max_tokens",
      label: "最大输出 tokens",
      type: "number",
      value: 8192,
    },
    {
      name: "image_max_bytes",
      label: "图片大小上限（字节）",
      type: "number",
      value: 524288,
    },
  ],
  batch: true,
});
