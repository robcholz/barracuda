import { configuration } from "../../../captive-portal/resources/web/ui/form";

export const mount = configuration({
  title: "QQ",
  endpoint: "/api/gateway/qq",
  description: "配置 QQ Bot 消息通道；替换当前运行时通道。",
  fields: [
    {
      name: "app_id",
      label: "App ID",
    },
    {
      name: "access_token",
      label: "Access Token",
      type: "password",
    },
    {
      name: "api_base",
      label: "API Base URL",
      type: "url",
      value: "https://api.sgroup.qq.com",
    },
  ],
});
