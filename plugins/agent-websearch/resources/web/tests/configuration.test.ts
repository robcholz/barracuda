import { testConfiguration } from "../../../../../tools/web/test-configuration";
import { mount } from "../entry";

testConfiguration(
  "agent-websearch",
  mount,
  "/api/tavily",
  {
    api_key: "secret",
  },
  {
    api_key: "secret",
    api_base: "https://api.tavily.com",
  },
);
