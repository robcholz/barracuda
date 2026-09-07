import { testConfiguration } from "../../../../../tools/web/test-configuration";
import { mount } from "../entry";

testConfiguration(
  "imessage-bluebubble",
  mount,
  "/api/gateway/bluebubbles",
  {
    server_url: "https://example.com",
    password: "secret",
  },
  {
    server_url: "https://example.com",
    password: "secret",
    use_private_api: true,
    stream_edit_min_delta_bytes: 128,
    stream_max_edits: 4,
  },
);
