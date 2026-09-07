import { configuration } from "../../../captive-portal/resources/web/ui/form";

export const mount = configuration({
  title: "Inkbox",
  endpoint: "/api/gateway/inkbox",
  description: "配置 Inkbox 身份与服务；替换当前运行时通道。",
  fields: [
    {
      name: "api_key",
      label: "API Key",
      type: "password",
    },
    {
      name: "identity_id",
      label: "Identity ID",
    },
    {
      name: "api_base",
      label: "API Base URL",
      type: "url",
      value: "https://inkbox.ai",
    },
  ],
});
