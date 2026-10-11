import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  json,
  pageHarness,
} from "../../../../captive-portal/resources/web/tests/page";
import type {
  PortalContext,
  Toast,
} from "../../../../captive-portal/resources/web/ui";
import { configuredRow, readChannel } from "../channel";

let harness: ReturnType<typeof pageHarness>;
beforeEach(() => {
  harness = pageHarness();
});
afterEach(async () => {
  await harness.close();
});

function context(lang: "zh" | "en" = "zh") {
  const controller = new AbortController();
  const toasts: Toast[] = [];
  const value: PortalContext = {
    signal: controller.signal,
    lang,
    toast: (toast) => {
      if (!controller.signal.aborted) toasts.push(toast);
    },
    navigate: () => {},
    has: () => true,
    status: () => null,
    refreshStatus: async () => {},
  };
  return { context: value, controller, toasts };
}

test("readChannel reads the configured flag; configuredRow shows the card once told", async () => {
  const { context: ctx } = context();
  harness.reply = async () =>
    json(200, {
      configured: true,
      signup: { email_address: "a@inkboxmail.com" },
    });
  expect(
    await readChannel<{ signup?: { email_address: string } }>(
      ctx,
      "/api/gateway/inkbox",
    ),
  ).toEqual({
    configured: true,
    signup: { email_address: "a@inkboxmail.com" },
  });
  expect(harness.calls.at(-1)?.method).toBe("GET");
  expect(harness.calls.at(-1)?.url).toBe("/api/gateway/inkbox/status");
  harness.reply = async () => json(200, { configured: "yes" });
  expect(await readChannel(ctx, "/api/gateway/qq")).toBeNull();
  harness.reply = async () => new Response(null, { status: 405 });
  expect(await readChannel(ctx, "/api/gateway/qq")).toBeNull();

  for (const lang of ["zh", "en"] as const) {
    const current = configuredRow("Telegram", lang);
    expect(current.element.hidden).toBe(true);
    current.show(true);
    expect(current.element.hidden).toBe(false);
    expect(current.element.querySelector(".bc-row__label")?.textContent).toBe(
      lang === "zh" ? "通道" : "Channel",
    );
    expect(current.element.querySelector(".bc-option-title")?.textContent).toBe(
      "Telegram",
    );
    // a stored configuration is a plain state: the bare check, no tile and no badge
    expect(current.element.querySelector("svg.bc-success")).not.toBeNull();
    expect(current.element.querySelector(".bc-option-icon")).toBeNull();
    expect(current.element.querySelector(".bc-badge")).toBeNull();
  }
});
