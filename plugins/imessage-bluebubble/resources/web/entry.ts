import { configuration } from "../../../captive-portal/resources/web/ui/form";

export const mount = configuration({
  title: "BlueBubbles",
  endpoint: "/api/gateway/bluebubbles",
  description: "连接 BlueBubbles 服务器；替换当前运行时通道。",
  fields: [
    {
      name: "server_url",
      label: "服务器 URL",
      type: "url",
    },
    {
      name: "password",
      label: "服务器密码",
      type: "password",
    },
    {
      name: "use_private_api",
      label: "使用 Private API",
      type: "checkbox",
      value: true,
    },
    {
      name: "stream_edit_min_delta_bytes",
      label: "流式编辑最小增量（字节）",
      type: "number",
      value: 128,
    },
    {
      name: "stream_max_edits",
      label: "最大流式编辑次数",
      type: "number",
      value: 4,
    },
  ],
});
