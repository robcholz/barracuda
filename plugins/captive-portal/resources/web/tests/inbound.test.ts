import { afterEach, beforeEach, expect, jest, test } from "bun:test";
import { json, pageHarness, type FetchCall } from "./page";
import {
  channelInbound,
  definePage,
  readChannel,
  settingsForm,
  type ChannelInboundOptions,
  type ChannelStatus,
  type OwnersReply,
} from "../ui";

const ENDPOINT = "/api/gateway/qq";

let harness: ReturnType<typeof pageHarness>;
/** What the device answers to `GET <endpoint>` and `GET <endpoint>/owners`. */
let status: ChannelStatus | null;
let owners: OwnersReply;
/** Overrides the reply to a POST (default 204). */
let post: (call: FetchCall) => Response;
const mounted: { unmount(): void }[] = [];

beforeEach(() => {
  jest.useFakeTimers();
  harness = pageHarness();
  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "receiving" },
    owners: { count: 1 },
  };
  owners = {
    owners: [{ id: "c2c:7F3A9B2E41D0", label: "QQ 用户" }],
    pairing: { code: "482913", expires_in: 581 },
    ignored: 0,
  };
  post = () => new Response(null, { status: 204 });
  harness.reply = async (call) => {
    if (call.method === "POST") return post(call);
    if (call.url === ENDPOINT)
      return status ? json(200, status) : new Response(null, { status: 204 });
    if (call.url === `${ENDPOINT}/owners`) return json(200, owners);
    return new Response(null, { status: 404 });
  };
});
afterEach(async () => {
  for (const page of mounted.splice(0)) page.unmount();
  jest.useRealTimers();
  await harness.close();
});

/** Lets pending fetches and their `.json()` resolve (`Bun.sleep` never ends under fake timers). */
async function flush() {
  for (let i = 0; i < 40; i++) await Promise.resolve();
}
async function advance(ms: number) {
  jest.advanceTimersByTime(ms);
  await flush();
}

/** A channel page as the five use it: a form, the rows before its fold, and the mount's read. */
async function render(
  lang: "zh" | "en" = "zh",
  options: Partial<ChannelInboundOptions> = {},
) {
  const mount = definePage((context) => {
    const form = settingsForm(
      {
        endpoint: ENDPOINT,
        submit: "Save",
        rows: [
          {
            title: "Bot",
            fields: [{ kind: "text", name: "app_id", label: "App ID" }],
          },
        ],
        advanced: {
          fields: [{ kind: "url", name: "api_base", label: "API Base URL" }],
        },
      },
      context,
    );
    const inbound = channelInbound(context, {
      endpoint: ENDPOINT,
      channel: "QQ",
      how: { zh: "在 QQ 里给机器人发送", en: "In QQ, send the bot" },
      ...options,
    });
    inbound.attach(form);
    void readChannel<ChannelStatus>(context, ENDPOINT).then(inbound.apply);
    return form.element;
  });
  const page = await harness.render(mount, lang);
  mounted.push(page);
  await flush();
  return page;
}
type Page = Awaited<ReturnType<typeof render>>;

const requests = () =>
  harness.calls.map((call) =>
    call.body === undefined
      ? `${call.method} ${call.url}`
      : `${call.method} ${call.url} ${JSON.stringify(call.body)}`,
  );
const rows = (page: Page) =>
  [...page.root.querySelectorAll<HTMLElement>(".bc-form > .bc-row")].map(
    (row) =>
      `${row.querySelector(".bc-title, .bc-disclosure")?.textContent}${row.style.display === "none" ? " (hidden)" : ""}`,
  );
const modeRow = (page: Page) =>
  [...page.root.querySelectorAll<HTMLElement>(".bc-row")].find((row) =>
    ["模式", "Mode"].includes(
      row.querySelector(".bc-title")?.textContent ?? "",
    ),
  )!;
const accountsRow = (page: Page) => modeRow(page).nextElementSibling!;
const stateLine = (page: Page) =>
  modeRow(page).querySelector<HTMLElement>("[role=status]")!;
const radio = (page: Page, mode: string) =>
  page.root.querySelector<HTMLInputElement>(
    `input[name="channel_mode"][value="${mode}"]`,
  )!;
const pick = async (page: Page, mode: string) => {
  radio(page, mode).click();
  await flush();
};
const press = async (page: Page, label: string) => {
  const target = [...page.root.querySelectorAll<HTMLElement>("button")].find(
    (node) => node.textContent === label,
  );
  if (!target) throw new Error(`no button «${label}»`);
  target.click();
  await flush();
};

test("a configured channel shows 模式 and 授权账号 before the fold, in the design's words", async () => {
  const page = await render("zh");
  expect(requests()).toEqual([`GET ${ENDPOINT}`, `GET ${ENDPOINT}/owners`]);
  expect(rows(page)).toEqual(["Bot", "模式", "授权账号", "高级"]);
  const mode = modeRow(page);
  expect(mode.querySelector(".bc-row__label")?.textContent).toBe(
    "模式选择设备怎样使用这个通道",
  );
  expect(
    [...mode.querySelectorAll(".bc-radio-card")].map(
      (card) => card.textContent,
    ),
  ).toEqual([
    "停用保留凭据，暂停通道",
    "仅发送Agent 主动发消息",
    "收发接收消息并回复",
  ]);
  expect(radio(page, "send_receive").checked).toBe(true);
  expect(stateLine(page).querySelector(".bc-badge--signal")?.textContent).toBe(
    "收发中",
  );
  expect(mode.querySelector<HTMLElement>(".bc-alert")?.style.display).toBe(
    "none",
  );

  const accounts = accountsRow(page);
  expect(accounts.querySelector(".bc-row__label")?.textContent).toBe(
    "授权账号能给设备下指令的账号",
  );
  const code = accounts.querySelector(".bc-page-title.bc-mono")!;
  expect(code.textContent).toBe("482913");
  expect(code.previousElementSibling?.textContent).toBe("在 QQ 里给机器人发送");
  expect(accounts.textContent).toContain("有效期还剩 9:41");
  expect(accounts.querySelector(".bc-mono:not(.bc-page-title)")).not.toBeNull();
  expect(accounts.textContent).toContain("换一个绑定码");
  expect(accounts.textContent).toContain("QQ 用户c2c:7F3A9B2E41D0移除");
  expect(
    [...accounts.querySelectorAll("button")].map((node) =>
      node.getAttribute("aria-label"),
    ),
  ).toEqual([null, "移除 QQ 用户"]);
});

test("in English, with the Telegram command and only the modes a channel offers", async () => {
  owners = {
    owners: [],
    pairing: { code: "012345", expires_in: 600 },
    ignored: 3,
  };
  const page = await render("en", {
    channel: "WeChat",
    modes: ["disabled", "send_receive"],
    how: "In Telegram, send the bot",
    command: (code) => `/start ${code}`,
  });
  expect(rows(page)).toEqual(["Bot", "Mode", "Allowed accounts", "Advanced"]);
  expect(modeRow(page).textContent).toContain(
    "Choose how the device uses this channel",
  );
  expect(
    [...modeRow(page).querySelectorAll(".bc-option-title")].map(
      (node) => node.textContent,
    ),
  ).toEqual(["Disabled", "Send and receive"]);
  expect(radio(page, "send")).toBeNull();
  expect(stateLine(page).textContent).toBe("Receiving");
  const accounts = accountsRow(page);
  expect(accounts.textContent).toContain(
    "Accounts that can command the device",
  );
  expect(accounts.querySelector(".bc-page-title")?.textContent).toBe(
    "/start 012345",
  );
  expect(accounts.textContent).toContain("Expires in 10:00");
  expect(accounts.textContent).toContain("New code");
  expect(accounts.textContent).toContain("No allowed accounts yet");
});

test("an unconfigured channel, or a reply without a mode, shows neither row", async () => {
  status = { configured: false };
  const page = await render("zh");
  expect(rows(page)).toEqual([
    "Bot",
    "模式 (hidden)",
    "授权账号 (hidden)",
    "高级",
  ]);
  expect(requests()).toEqual([`GET ${ENDPOINT}`]);
  await advance(10_000);
  expect(requests()).toEqual([`GET ${ENDPOINT}`]);

  page.unmount();
  harness.calls.length = 0;
  status = null;
  const silent = await render("zh");
  expect(rows(silent)).toEqual([
    "Bot",
    "模式 (hidden)",
    "授权账号 (hidden)",
    "高级",
  ]);
});

test("the receive state badge: connecting, waiting for a slot with the limit, disconnected, send only, disabled", async () => {
  const cases: [ChannelStatus, string, boolean][] = [
    [
      {
        configured: true,
        mode: "send_receive",
        receive: { state: "starting" },
      },
      "连接中",
      false,
    ],
    [
      { configured: true, mode: "send_receive", receive: { state: "idle" } },
      "连接中",
      false,
    ],
    [{ configured: true, mode: "send", owners: { count: 1 } }, "仅发送", false],
    [
      { configured: true, mode: "disabled", owners: { count: 1 } },
      "已停用",
      false,
    ],
  ];
  for (const [state, label, signal] of cases) {
    status = state;
    const page = await render("zh");
    expect(stateLine(page).textContent).toBe(label);
    expect(stateLine(page).querySelector(".bc-badge--signal") !== null).toBe(
      signal,
    );
    expect(radio(page, state.mode!).checked).toBe(true);
    page.unmount();
  }

  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "no_slot", capacity: 2 },
    owners: { count: 1 },
  };
  const full = await render("zh");
  const slots = stateLine(full).querySelector(".bc-term")!;
  expect(slots.classList.contains("bc-mono")).toBe(true);
  expect(slots.getAttribute("tabindex")).toBe("0");
  expect(slots.firstChild?.textContent).toBe("名额 2/2");
  expect(slots.querySelector(".bc-tooltip")?.textContent).toBe(
    "每个收发通道保持一条连接，名额由设备内存决定",
  );
  expect(stateLine(full).querySelector(".bc-badge")?.textContent).toBe(
    "等待名额",
  );
  const alert = modeRow(full).querySelector<HTMLElement>(".bc-alert")!;
  expect(alert.style.display).toBe("");
  expect(alert.querySelector(".bc-alert__title")?.textContent).toBe(
    "收发通道已达上限（2）",
  );
  expect(alert.textContent).toContain(
    "把其他通道改为仅发送或停用后，QQ 会自动开始接收。",
  );
  full.unmount();

  const fullEn = await render("en", { channel: "WeChat" });
  expect(stateLine(fullEn).textContent).toContain(
    "Waiting for a slotSlots 2 of 2",
  );
  expect(modeRow(fullEn).querySelector(".bc-alert")?.textContent).toBe(
    "Receive slots are full (2)Set another channel to Send only or Disabled and WeChat starts receiving on its own.",
  );
  fullEn.unmount();
  const fullWechat = await render("zh", { channel: "微信" });
  expect(modeRow(fullWechat).querySelector(".bc-alert")?.textContent).toContain(
    "停用后，微信会自动开始接收。",
  );
  fullWechat.unmount();

  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "error", message: "getUpdates: 409 Conflict" },
    owners: { count: 1 },
  };
  const broken = await render("en");
  expect(stateLine(broken).querySelector(".bc-badge")?.textContent).toBe(
    "Disconnected",
  );
  expect(stateLine(broken).querySelector(".bc-mono")?.textContent).toBe(
    "getUpdates: 409 Conflict",
  );
});

test("the slots in use follow the receive badge whenever the device reports them", async () => {
  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "receiving", slots: { in_use: 1, capacity: 2 } },
    owners: { count: 1 },
  };
  const receiving = await render("zh");
  expect(stateLine(receiving).textContent).toStartWith("收发中名额 1/2");
  const slots = stateLine(receiving).querySelector(".bc-term")!;
  expect(slots.classList.contains("bc-mono")).toBe(true);
  expect(slots.getAttribute("tabindex")).toBe("0");
  expect(slots.firstChild?.textContent).toBe("名额 1/2");
  expect(slots.querySelector(".bc-tooltip")?.textContent).toBe(
    "每个收发通道保持一条连接，名额由设备内存决定",
  );
  expect(
    modeRow(receiving).querySelector<HTMLElement>(".bc-alert")!.style.display,
  ).toBe("none");
  receiving.unmount();

  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "starting", slots: { in_use: 2, capacity: 2 } },
    owners: { count: 1 },
  };
  const starting = await render("en");
  expect(stateLine(starting).textContent).toStartWith("ConnectingSlots 2 of 2");
  starting.unmount();

  status = {
    configured: true,
    mode: "send_receive",
    receive: {
      state: "error",
      message: "Conflict: terminated by other getUpdates request",
      slots: { in_use: 1, capacity: 3 },
    },
    owners: { count: 1 },
  };
  const broken = await render("zh");
  const line = stateLine(broken);
  expect(line.querySelector(".bc-badge")?.textContent).toBe("连接中断");
  expect(line.querySelector(".bc-term")?.firstChild?.textContent).toBe(
    "名额 1/3",
  );
  expect(line.lastElementChild?.textContent).toBe(
    "Conflict: terminated by other getUpdates request",
  );
  broken.unmount();

  // the limit the device reports with no_slot; the alert stays
  status = {
    configured: true,
    mode: "send_receive",
    receive: {
      state: "no_slot",
      capacity: 2,
      slots: { in_use: 2, capacity: 2 },
    },
    owners: { count: 1 },
  };
  const full = await render("zh");
  expect(stateLine(full).querySelectorAll(".bc-term")).toHaveLength(1);
  expect(
    stateLine(full).querySelector(".bc-term")?.firstChild?.textContent,
  ).toBe("名额 2/2");
  const alert = modeRow(full).querySelector<HTMLElement>(".bc-alert")!;
  expect(alert.style.display).toBe("");
  expect(alert.querySelector(".bc-alert__title")?.textContent).toBe(
    "收发通道已达上限（2）",
  );
  full.unmount();

  // a webhook channel holds no slot, and send only shows none
  for (const state of [
    {
      configured: true,
      mode: "send_receive",
      receive: { state: "receiving" },
    },
    { configured: true, mode: "send" },
  ] satisfies ChannelStatus[]) {
    status = { ...state, owners: { count: 1 } };
    const page = await render("zh");
    expect(stateLine(page).querySelector(".bc-term")).toBeNull();
    page.unmount();
  }
});

test("choosing a mode posts it at once, reads the channel again and refreshes the shell's status", async () => {
  const page = await render("zh");
  harness.calls.length = 0;
  status = { configured: true, mode: "send", owners: { count: 1 } };
  await pick(page, "send");
  expect(requests()).toEqual([
    `POST ${ENDPOINT}/mode {"mode":"send"}`,
    `GET ${ENDPOINT}`,
  ]);
  expect(page.refreshes.count).toBe(1);
  expect(page.toasts).toEqual([]);
  expect(stateLine(page).textContent).toBe("仅发送");
  // 「清空」 keeps the device's mode
  page.root.querySelector("form")!.reset();
  expect(radio(page, "send").checked).toBe(true);
});

test("a full receive pool (409 no_slot) still saves the mode and shows the limit", async () => {
  status = { configured: true, mode: "send", owners: { count: 1 } };
  const page = await render("zh");
  harness.calls.length = 0;
  post = () => json(409, { error: "no_slot", capacity: 2 });
  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "no_slot", capacity: 2 },
    owners: { count: 1 },
  };
  await pick(page, "send_receive");
  expect(requests()).toEqual([
    `POST ${ENDPOINT}/mode {"mode":"send_receive"}`,
    `GET ${ENDPOINT}`,
  ]);
  expect(page.toasts).toEqual([]);
  expect(radio(page, "send_receive").checked).toBe(true);
  expect(modeRow(page).querySelector(".bc-alert__title")?.textContent).toBe(
    "收发通道已达上限（2）",
  );
  expect(page.refreshes.count).toBe(1);
});

test("a refused mode is toasted and the choice goes back", async () => {
  const page = await render("zh");
  harness.calls.length = 0;
  post = () => json(422, { error: "registration_failed" });
  await pick(page, "disabled");
  expect(requests()).toEqual([`POST ${ENDPOINT}/mode {"mode":"disabled"}`]);
  expect(radio(page, "send_receive").checked).toBe(true);
  expect(page.toasts.at(-1)).toMatchObject({
    kind: "error",
    title: "配置被拒绝",
    code: "422",
  });
  expect(page.refreshes.count).toBe(0);
});

test("while receiving, the channel is read every 5 s; the badge, the shell and the list follow", async () => {
  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "starting" },
    owners: { count: 1 },
  };
  const page = await render("zh");
  harness.calls.length = 0;
  await advance(4_999);
  expect(requests()).toEqual([]);
  status = {
    configured: true,
    mode: "send_receive",
    receive: { state: "receiving" },
    owners: { count: 1 },
  };
  await advance(1);
  expect(requests()).toEqual([`GET ${ENDPOINT}`]);
  expect(stateLine(page).textContent).toBe("收发中");
  expect(page.refreshes.count).toBe(1);

  // someone paired: the list and the rotated code are read again
  owners = {
    owners: [...owners.owners, { id: "c2c:00AA", label: null }],
    pairing: { code: "730055", expires_in: 600 },
    ignored: 0,
  };
  status = { ...status, owners: { count: 2 } };
  harness.calls.length = 0;
  await advance(5_000);
  expect(requests()).toEqual([`GET ${ENDPOINT}`, `GET ${ENDPOINT}/owners`]);
  expect(accountsRow(page).querySelector(".bc-page-title")?.textContent).toBe(
    "730055",
  );
  // an account without a label shows its ID, in mono
  const unnamed = [
    ...accountsRow(page).querySelectorAll(".bc-option-title"),
  ].at(-1)!;
  expect([unnamed.textContent, unnamed.classList.contains("bc-mono")]).toEqual([
    "c2c:00AA",
    true,
  ]);
  expect(page.refreshes.count).toBe(1);

  // send only: no more reads
  status = { configured: true, mode: "send", owners: { count: 2 } };
  await advance(5_000);
  harness.calls.length = 0;
  await advance(20_000);
  expect(requests()).toEqual([]);
});

test("a missed read keeps the rows and asks again; leaving the page stops the reads", async () => {
  const page = await render("zh");
  harness.calls.length = 0;
  const before = harness.reply;
  harness.reply = async () => {
    throw new TypeError("Failed to fetch");
  };
  await advance(5_000);
  expect(rows(page)).toEqual(["Bot", "模式", "授权账号", "高级"]);
  harness.reply = before;
  await advance(5_000);
  expect(requests()).toEqual([`GET ${ENDPOINT}`, `GET ${ENDPOINT}`]);
  page.unmount();
  harness.calls.length = 0;
  await advance(30_000);
  expect(requests()).toEqual([]);
});

test("the code counts down locally and is read again when it expires", async () => {
  status = { configured: true, mode: "send", owners: { count: 1 } };
  owners = { ...owners, pairing: { code: "482913", expires_in: 3 } };
  const page = await render("zh");
  harness.calls.length = 0;
  expect(accountsRow(page).textContent).toContain("有效期还剩 0:03");
  await advance(1_000);
  expect(accountsRow(page).textContent).toContain("有效期还剩 0:02");
  expect(requests()).toEqual([]);
  owners = { ...owners, pairing: { code: "555000", expires_in: 600 } };
  await advance(2_000);
  expect(requests()).toEqual([`GET ${ENDPOINT}/owners`]);
  expect(accountsRow(page).querySelector(".bc-page-title")?.textContent).toBe(
    "555000",
  );
  expect(accountsRow(page).textContent).toContain("有效期还剩 10:00");
});

test("换一个绑定码 and 移除 post at once, without asking, and read the list again", async () => {
  status = { configured: true, mode: "send", owners: { count: 1 } };
  const page = await render("zh");
  harness.calls.length = 0;
  owners = { ...owners, pairing: { code: "100200", expires_in: 600 } };
  await press(page, "换一个绑定码");
  expect(requests()).toEqual([
    `POST ${ENDPOINT}/owners {"rotate":true}`,
    `GET ${ENDPOINT}/owners`,
  ]);
  expect(accountsRow(page).querySelector(".bc-page-title")?.textContent).toBe(
    "100200",
  );

  harness.calls.length = 0;
  owners = { ...owners, owners: [] };
  await press(page, "移除");
  expect(requests()).toEqual([
    `POST ${ENDPOINT}/owners {"remove":"c2c:7F3A9B2E41D0"}`,
    `GET ${ENDPOINT}/owners`,
  ]);
  expect(accountsRow(page).textContent).toContain("还没有授权账号");
  expect(page.toasts).toEqual([]);

  // no entropy: the device's refusal is toasted
  post = () => json(503, { error: "entropy_unavailable" });
  await press(page, "换一个绑定码");
  expect(page.toasts.at(-1)).toMatchObject({
    kind: "error",
    title: "提交失败",
    code: "503",
  });
});

test("pairing: null (a full list) hides the code box", async () => {
  status = { configured: true, mode: "send", owners: { count: 1 } };
  owners = { ...owners, pairing: null };
  const page = await render("zh");
  const accounts = accountsRow(page);
  expect(accounts.querySelector(".bc-page-title")).toBeNull();
  expect(accounts.textContent).not.toContain("换一个绑定码");
  expect(accounts.textContent).toContain("QQ 用户");
  const first = accounts.querySelector<HTMLElement>(".bc-frame > div")!;
  expect(first.style.borderTop).toBe("");
});
