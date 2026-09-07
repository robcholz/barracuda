import { testConfiguration } from "../../../../../tools/web/test-configuration";
import { mount } from "../entry";

testConfiguration(
  "imessage-wechat",
  mount,
  "/api/gateway/wechat",
  {
    token: "secret",
  },
  {
    token: "secret",
    api_base: "https://ilinkai.weixin.qq.com",
    app_id: "bot",
    client_version: "131329",
    x_wechat_uin: "MA==",
  },
);
