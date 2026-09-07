import { configuration } from "../../../captive-portal/resources/web/ui/form";

export const mount = configuration({
  title: "Telegram",
  endpoint: "/api/gateway/telegram",
  description: "配置 Telegram Bot；替换当前运行时通道。",
  fields: [
    {
      name: "token",
      label: "Bot Token",
      type: "password",
    },
    {
      name: "api_base",
      label: "API Base URL",
      type: "url",
      value: "https://api.telegram.org",
    },
    {
      name: "draft_min_delta_bytes",
      label: "草稿最小增量（字节）",
      type: "number",
      value: 24,
    },
  ],
});
