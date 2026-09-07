import { testConfiguration } from "../../../../../tools/web/test-configuration";
import { mount } from "../entry";

testConfiguration(
  "imessage-telegram",
  mount,
  "/api/gateway/telegram",
  {
    token: "secret",
  },
  {
    token: "secret",
    api_base: "https://api.telegram.org",
    draft_min_delta_bytes: 24,
  },
);
