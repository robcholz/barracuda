import { configuration } from "../../../captive-portal/resources/web/ui/form";

export const mount = configuration({
  title: "网页搜索",
  endpoint: "/api/tavily",
  description: "配置 Tavily 搜索服务；配置保存到设备存储。",
  fields: [
    {
      name: "api_key",
      label: "Tavily API Key",
      type: "password",
    },
    {
      name: "api_base",
      label: "API Base URL",
      type: "url",
      value: "https://api.tavily.com",
    },
  ],
});
