import { testConfiguration } from "../../../../../tools/web/test-configuration";
import { mount } from "../entry";

testConfiguration(
  "imessage-qq",
  mount,
  "/api/gateway/qq",
  {
    app_id: "app",
    access_token: "secret",
  },
  {
    app_id: "app",
    access_token: "secret",
    api_base: "https://api.sgroup.qq.com",
  },
);
