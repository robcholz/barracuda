import { testConfiguration } from "../../../../../tools/web/test-configuration";
import { mount } from "../entry";

testConfiguration(
  "agent",
  mount,
  "/api/model-api",
  {
    api_key: "secret",
    model: "model",
    base_url: "https://example.com",
  },
  [
    {
      api_key: "secret",
      model: "model",
      base_url: "https://example.com",
      backend: "openai_compatible",
      purpose: "root_agent",
      default: true,
      timeout_ms: 120000,
      max_tokens: 8192,
      image_max_bytes: 524288,
    },
  ],
);
