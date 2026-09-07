import { configuration } from "../../../captive-portal/resources/web/ui/form";

export const mount = configuration({
  title: "微信",
  endpoint: "/api/gateway/wechat",
  description: "配置微信消息通道；替换当前运行时通道。",
  fields: [
    {
      name: "token",
      label: "Token",
      type: "password",
    },
    {
      name: "api_base",
      label: "API Base URL",
      type: "url",
      value: "https://ilinkai.weixin.qq.com",
    },
    {
      name: "app_id",
      label: "App ID",
      value: "bot",
    },
    {
      name: "client_version",
      label: "客户端版本",
      value: "131329",
    },
    {
      name: "x_wechat_uin",
      label: "X-Wechat-UIN",
      value: "MA==",
    },
    {
      name: "route_tag",
      label: "路由标签（可选）",
      optional: true,
    },
  ],
});
