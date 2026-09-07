import { testConfiguration } from "../../../../../tools/web/test-configuration";
import { mount } from "../entry";

testConfiguration(
  "imessage-inkbox",
  mount,
  "/api/gateway/inkbox",
  {
    api_key: "secret",
    identity_id: "identity",
  },
  {
    api_key: "secret",
    identity_id: "identity",
    api_base: "https://inkbox.ai",
  },
);
