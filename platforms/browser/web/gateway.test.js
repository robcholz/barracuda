import { expect, test } from "bun:test";

import { gatewayUrl, parseGatewayUrl } from "./gateway.js";

test("encodes arbitrary System ports and paths", () => {
  const url = gatewayUrl(
    4321,
    "/custom/ui/?mode=test",
    "https://example.com/device/index.html",
  );
  expect(url.href).toBe(
    "https://example.com/device/_barracuda/4321/custom/ui/?mode=test",
  );
  expect(parseGatewayUrl(url, "https://example.com/device/")).toEqual({
    port: 4321,
    pathname: "/custom/ui/",
  });
});

test("rejects routes outside the Browser gateway", () => {
  expect(
    parseGatewayUrl(
      "https://example.com/device/portal/",
      "https://example.com/device/",
    ),
  ).toBeUndefined();
  expect(() => gatewayUrl(0, "/", "https://example.com/")).toThrow();
});
