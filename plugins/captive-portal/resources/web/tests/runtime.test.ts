import { expect, test } from "bun:test";
import {
  ModuleSession,
  parseEntries,
  parseStatus,
  sortEntries,
  type PortalContext,
  type Toast,
  type WebEntry,
} from "../src/runtime";

export const entry: WebEntry = {
  id: "wifi",
  group: "device",
  order: 10,
  title: { zh: "Wi-Fi", en: "Wi-Fi" },
  summary: {
    zh: "扫描、连接或忘记无线网络",
    en: "Scan, join or forget wireless networks",
  },
  icon: "/portal/assets/wifi/icon.svg",
  figure: "/portal/assets/wifi/figure.js",
  module: "/portal/assets/wifi/entry.js",
};

test("manifest accepts the full record, null assets, and keeps only known fields", () => {
  expect(parseEntries([{ ...entry, extra: "ignored" }])).toEqual([entry]);
  const bare = {
    ...entry,
    id: "imessage-qq",
    group: "channel",
    icon: null,
    figure: null,
    module: "/portal/assets/imessage-qq/entry.js",
  };
  expect(parseEntries([bare])).toEqual([bare as WebEntry]);
});

test("manifest rejects foreign or traversing URLs, bad groups, orders, labels and duplicate IDs", () => {
  const bad: unknown[] = [];
  for (const url of [
    "https://evil.test/a.js",
    "/portal/assets/other/a.js",
    "/portal/assets/wifi/../a.js",
    "/portal/assets/wifi/%2e/a.js",
    "/portal/assets/wifi/",
    "/portal/assets/wifi/a.js?x=1",
  ])
    bad.push(
      { ...entry, module: url },
      { ...entry, icon: url },
      { ...entry, figure: url },
    );
  bad.push(
    { ...entry, icon: undefined },
    { ...entry, group: "settings" },
    { ...entry, order: 1.5 },
    { ...entry, order: 256 },
    { ...entry, title: "Wi-Fi" },
    { ...entry, summary: { zh: "只有中文" } },
    { ...entry, id: "Wi-Fi" },
    { ...entry, id: "a".repeat(65) },
  );
  for (const record of bad) expect(() => parseEntries([record])).toThrow();
  expect(() => parseEntries([entry, entry])).toThrow();
  expect(() => parseEntries({})).toThrow();
});

test("entries sort by group, then order, then ID", () => {
  const make = (id: string, group: WebEntry["group"], order: number) => ({
    ...entry,
    id,
    group,
    order,
  });
  const sorted = sortEntries([
    make("imessage-web", "channel", 10),
    make("b", "agent", 20),
    make("a", "agent", 20),
    make("agent", "agent", 10),
    make("wifi", "device", 90),
  ]);
  expect(sorted.map((item) => item.id)).toEqual([
    "wifi",
    "agent",
    "a",
    "b",
    "imessage-web",
  ]);
});

function host(lang: "zh" | "en" = "zh") {
  const toasts: Toast[] = [];
  const routes: string[] = [];
  const asked: string[] = [];
  const refreshes = { count: 0 };
  return {
    toasts,
    routes,
    asked,
    refreshes,
    host: {
      lang,
      toast: (toast: Toast) => toasts.push(toast),
      navigate: (id: string) => routes.push(id),
      status: (id: string) => {
        asked.push(id);
        return id === "wifi" ? { state: "ready" as const } : null;
      },
      refreshStatus: async () => {
        refreshes.count++;
      },
    },
  };
}

test("a mount gets the language, toast and navigate; all go quiet once it is left", async () => {
  const session = new ModuleSession();
  const { host: shell, toasts, routes } = host("en");
  let context: PortalContext | undefined;
  let disposed = 0;
  let imported = 0;
  const mounted = await session.open(entry, {} as HTMLElement, shell, {
    load: async () => ({
      mount: (_root: HTMLElement, value: PortalContext) => {
        context = value;
        value.toast({ kind: "info", title: "hello" });
        value.navigate("agent");
        return () => {
          disposed++;
        };
      },
    }),
    onImported: () => imported++,
  });
  expect(mounted).toBe(true);
  expect(imported).toBe(1);
  expect(context?.lang).toBe("en");
  expect(Object.isFrozen(context)).toBe(true);
  expect(toasts).toEqual([{ kind: "info", title: "hello" }]);
  expect(routes).toEqual(["agent"]);
  session.close();
  expect(context?.signal.aborted).toBe(true);
  expect(disposed).toBe(1);
  context?.toast({ kind: "error", title: "late" });
  context?.navigate("overview");
  expect(toasts).toHaveLength(1);
  expect(routes).toHaveLength(1);
  session.close();
  expect(disposed).toBe(1);
});

test("an import finishing after navigation does not mount", async () => {
  const session = new ModuleSession();
  let release!: (value: unknown) => void;
  let mounted = false;
  const pending = session.open(entry, {} as HTMLElement, host().host, {
    load: () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  });
  session.close();
  release({
    mount: () => {
      mounted = true;
    },
  });
  expect(await pending).toBe(false);
  expect(mounted).toBe(false);
});

test("late async mount cleanup is not lost", async () => {
  const session = new ModuleSession();
  let release!: (cleanup: () => void) => void;
  let cleaned = false;
  const pending = session.open(entry, {} as HTMLElement, host().host, {
    load: async () => ({
      mount: () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    }),
  });
  await Bun.sleep(0);
  session.close();
  release(() => {
    cleaned = true;
  });
  expect(await pending).toBe(false);
  expect(cleaned).toBe(true);
});

test("a module without mount, or with an invalid cleanup, fails", async () => {
  const session = new ModuleSession();
  await expect(
    session.open(entry, {} as HTMLElement, host().host, {
      load: async () => ({}),
    }),
  ).rejects.toThrow(/mount/);
  await expect(
    session.open(entry, {} as HTMLElement, host().host, {
      load: async () => ({ mount: () => 42 }),
    }),
  ).rejects.toThrow(/cleanup/);
  await expect(
    session.open(entry, {} as HTMLElement, host().host, {
      load: async () => {
        throw new Error("offline");
      },
    }),
  ).rejects.toThrow("offline");
});

test("status: valid records are kept, malformed ones left out, a non-object rejected", () => {
  const status = parseStatus({
    entries: {
      wifi: {
        state: "ready",
        label: { zh: "已连接", en: "Connected" },
        detail: "HomeNet",
        extra: 1,
      },
      agent: { state: "off", label: { zh: "未配置", en: "Not set up" } },
      "imessage-web": { state: "ready" },
      "bad state": { state: "ready" },
      odd: { state: "busy" },
      half: { state: "ready", label: { zh: "只有中文" } },
      empty: { state: "attention", detail: "" },
      long: { state: "attention", detail: "x".repeat(129) },
      number: { state: "ready", detail: 7 },
    },
  });
  expect([...status.keys()]).toEqual(["wifi", "agent", "imessage-web"]);
  expect(status.get("wifi")).toEqual({
    state: "ready",
    label: { zh: "已连接", en: "Connected" },
    detail: "HomeNet",
  });
  expect(status.get("imessage-web")).toEqual({ state: "ready" });
  for (const value of [null, [], {}, { entries: [] }, { entries: "x" }])
    expect(() => parseStatus(value)).toThrow();
  expect(parseStatus({ entries: {} }).size).toBe(0);
});

test("a mount reads its own status by default and asks the shell to refresh it, until it is left", async () => {
  const session = new ModuleSession();
  const { host: shell, asked, refreshes } = host();
  let context: PortalContext | undefined;
  await session.open(entry, {} as HTMLElement, shell, {
    load: async () => ({
      mount: (_root: HTMLElement, value: PortalContext) => {
        context = value;
      },
    }),
  });
  expect(context?.status()).toEqual({ state: "ready" });
  expect(context?.status("agent")).toBeNull();
  expect(asked).toEqual(["wifi", "agent"]);
  await context?.refreshStatus();
  expect(refreshes.count).toBe(1);
  session.close();
  await context?.refreshStatus();
  expect(refreshes.count).toBe(1);
});
