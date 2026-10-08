import {
  ICON_CHEVRON_DOWN,
  ICON_CHEVRON_RIGHT,
  ICON_PLUS,
  ICON_REFRESH,
  KIT_STRINGS,
  badge,
  button,
  fieldControl,
  h,
  header,
  icon,
  kv,
  page,
  submitJson,
  twoDigits,
  type Lang,
  type PortalContext,
  type PortalModule,
} from "../../../captive-portal/resources/web/ui";

/** `GET /api/wifi` (plugins/wifi/crates/plugin/src/endpoint.rs `StatusResponse`). */
export interface WifiStatus {
  capabilities: {
    access_point: boolean;
    scanning: boolean;
    station_configuration: boolean;
  };
  station: {
    state: "disconnected" | "connecting" | "connected";
    ssid?: string;
  };
  access_point: { state: "stopped" | "starting" | "started"; ssid?: string };
}

/** One item of `GET /api/wifi/scan` (`NetworkResponse`). */
export interface VisibleNetwork {
  ssid: string;
  signal_dbm: number | null;
  secured: boolean;
}

const STATUS_URL = "/api/wifi";
const SCAN_URL = "/api/wifi/scan";
/** The phone layout, as the shell switches it (`PHONE_QUERY`). */
const PHONE_QUERY = "(max-width: 719px)";
/** The manual-entry form's key in the open-form state (an SSID is never empty). */
const MANUAL = "";
/** Joining waits for the radio to associate and get an address. */
const JOIN_TIMEOUT_MS = 30_000;
const SCAN_TIMEOUT_MS = 20_000;
const STATUS_TIMEOUT_MS = 10_000;
/** Desktop table columns: signal, network, security, strength. */
const COLUMNS = "56px minmax(0, 1fr) 120px 96px";
/** Lucide 0.469 `lock` (ISC). */
const ICON_LOCK =
  '<rect width="18" height="11" x="3" y="11" rx="2" ry="2"></rect><path d="M7 11V7a5 5 0 0 1 10 0v4"></path>';

/** The page's copy: the design's WIFI_T (desktop) and MOB_T (phone) tables. */
export const STRINGS = {
  zh: {
    lead: "扫描、连接或忘记无线网络。",
    status: "状态",
    connected: "已连接",
    connecting: "正在连接…",
    disconnected: "未连接",
    network: "网络",
    hotspot: "配置热点",
    off: "已关闭",
    starting: "正在开启…",
    nearby: "附近的网络",
    rescan: "重新扫描",
    scanning: "正在扫描…",
    hSignal: "信号",
    hNetwork: "网络",
    hSecurity: "安全",
    hStrength: "强度",
    current: "当前",
    open: "开放",
    secured: "加密",
    none: "没有发现网络",
    managed: "网络由当前平台管理",
    ssid: "网络名称",
    password: "密码",
    passwordOpen: "密码（开放网络留空）",
    join: "连接",
    joinHint: "连接后，在新网络中重新打开门户。",
    manual: "手动输入网络名称",
    forget: "忘记此网络",
    forgetDesc: (ssid: string) => `设备会断开 ${ssid}，并开启配置热点`,
    forgetBtn: "忘记网络",
    phoneCurrent: "当前连接",
    phoneForget: (ssid: string) => `忘记 ${ssid}`,
    phoneForgetHint: "忘记后设备会开启配置热点",
    end: "。",
    joined: (ssid: string) => `已连接 ${ssid}`,
    forgot: (ssid: string) => `已忘记 ${ssid}`,
    statusFailed: "无法读取 Wi-Fi 状态",
    scanFailed: "扫描失败",
    ssidLength: "网络名称最多 32 字节。",
    passwordLength: "请输入 8 到 63 个字符的密码。",
  },
  en: {
    lead: "Scan, join or forget wireless networks.",
    status: "Status",
    connected: "Connected",
    connecting: "Connecting…",
    disconnected: "Not connected",
    network: "Network",
    hotspot: "Setup hotspot",
    off: "Off",
    starting: "Starting…",
    nearby: "Nearby networks",
    rescan: "Rescan",
    scanning: "Scanning…",
    hSignal: "Signal",
    hNetwork: "Network",
    hSecurity: "Security",
    hStrength: "Strength",
    current: "Current",
    open: "Open",
    secured: "Secured",
    none: "No networks found",
    managed: "The platform manages the network",
    ssid: "Network name",
    password: "Password",
    passwordOpen: "Password (leave empty for an open network)",
    join: "Join",
    joinHint: "After joining, reopen the portal on the new network.",
    manual: "Enter a network name",
    forget: "Forget this network",
    forgetDesc: (ssid: string) =>
      `The device leaves ${ssid} and turns on the setup hotspot`,
    forgetBtn: "Forget network",
    phoneCurrent: "Current connection",
    phoneForget: (ssid: string) => `Forget ${ssid}`,
    phoneForgetHint: "The device then turns on the setup hotspot",
    end: ".",
    joined: (ssid: string) => `Connected to ${ssid}`,
    forgot: (ssid: string) => `Forgot ${ssid}`,
    statusFailed: "Couldn't read the Wi-Fi status",
    scanFailed: "Scan failed",
    ssidLength: "Use at most 32 bytes for the network name.",
    passwordLength: "Enter a password of 8 to 63 characters.",
  },
} as const;

type Strings = (typeof STRINGS)[Lang];

const bytes = (value: string) => new TextEncoder().encode(value).length;

/** Signal bars (0 to 4) from dBm, with the design's thresholds; an unknown signal shows none. */
export function signalBars(dbm: number | null): number {
  if (dbm === null) return 0;
  return dbm >= -55 ? 4 : dbm >= -65 ? 3 : dbm >= -75 ? 2 : 1;
}

/**
 * The scan as the list shows it: hidden networks (no SSID) dropped, one row per SSID (its strongest
 * access point), strongest first.
 */
export function nearbyNetworks(networks: readonly VisibleNetwork[]) {
  const strongest = new Map<string, VisibleNetwork>();
  for (const network of networks) {
    if (!network.ssid) continue;
    const seen = strongest.get(network.ssid);
    if (!seen || (network.signal_dbm ?? -128) > (seen.signal_dbm ?? -128))
      strongest.set(network.ssid, network);
  }
  return [...strongest.values()].sort(
    (left, right) => (right.signal_dbm ?? -128) - (left.signal_dbm ?? -128),
  );
}

function isStatus(value: unknown): value is WifiStatus {
  const status = value as Partial<WifiStatus> | null;
  return (
    !!status &&
    typeof status.capabilities === "object" &&
    typeof status.station?.state === "string" &&
    typeof status.access_point?.state === "string"
  );
}

function isScan(value: unknown): value is VisibleNetwork[] {
  return (
    Array.isArray(value) &&
    value.every(
      (item) =>
        !!item &&
        typeof item.ssid === "string" &&
        typeof item.secured === "boolean" &&
        (item.signal_dbm === null || typeof item.signal_dbm === "number"),
    )
  );
}

type Fetched =
  { ok: true; value: unknown } | { ok: false; status: number | null } | null;

/** GETs JSON with the page's lifetime and a timeout; `null` once the page is gone. */
async function getJson(
  context: PortalContext,
  url: string,
  timeoutMs: number,
): Promise<Fetched> {
  const request = new AbortController();
  const cancel = () => request.abort();
  context.signal.addEventListener("abort", cancel, { once: true });
  const timeout = setTimeout(cancel, timeoutMs);
  try {
    const response = await fetch(url, {
      cache: "no-store",
      redirect: "error",
      signal: request.signal,
    });
    if (!response.ok)
      return context.signal.aborted
        ? null
        : { ok: false, status: response.status };
    const value: unknown = await response.json();
    return context.signal.aborted ? null : { ok: true, value };
  } catch {
    return context.signal.aborted ? null : { ok: false, status: null };
  } finally {
    clearTimeout(timeout);
    context.signal.removeEventListener("abort", cancel);
  }
}

/** The design's four-bar signal glyph. */
function bars(level: number): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 20 16");
  svg.setAttribute("width", "20");
  svg.setAttribute("height", "16");
  svg.setAttribute("aria-hidden", "true");
  svg.style.flex = "none";
  const shapes = [
    [0, 11, 5],
    [5.5, 8, 8],
    [11, 4.5, 11.5],
    [16.5, 1, 15],
  ];
  shapes.forEach(([x, y, height], index) => {
    const rect = document.createElementNS(ns, "rect");
    for (const [name, value] of Object.entries({
      x,
      y,
      width: 3,
      height,
      rx: 0.5,
    }))
      rect.setAttribute(name, String(value));
    rect.style.fill = `var(--${index < level ? "foreground" : "border"})`;
    svg.append(rect);
  });
  return svg;
}

interface JoinForm {
  element: HTMLFormElement;
  focus(): void;
  clear(): void;
}

/** The page: header with status and the router, nearby networks, forget. */
export const mount: PortalModule["mount"] = (root, context) => {
  const { lang, signal } = context;
  if (signal.aborted) return;
  const t: Strings = STRINGS[lang];
  const kit = KIT_STRINGS[lang];

  let status: WifiStatus | null = null;
  let networks: VisibleNetwork[] | null = null;
  let scanning = false;
  let busy = false;
  /** The SSID whose join form is open, {@link MANUAL} for the manual form, or `null`. */
  let picked: string | null = null;
  const forms = new Map<string, JoinForm>();
  const media =
    typeof matchMedia === "function" ? matchMedia(PHONE_QUERY) : null;
  let phone = media?.matches ?? false;

  const canJoin = () => !!status?.capabilities.station_configuration;
  const isCurrent = (network: VisibleNetwork) =>
    status?.station.state === "connected" &&
    status.station.ssid === network.ssid;

  const mono = (text: string, style = "") =>
    h("span", { class: "bc-mono", style: style || undefined }, text);

  function stateBadge() {
    if (!status) return "—";
    if (status.station.state === "connected")
      return badge(t.connected, lang, { signal: true });
    return badge(
      status.station.state === "connecting" ? t.connecting : t.disconnected,
      lang,
    );
  }

  function hotspotValue() {
    if (!status || !status.capabilities.access_point) return "—";
    const ap = status.access_point;
    if (ap.state === "started") return ap.ssid ? mono(ap.ssid) : "—";
    return ap.state === "starting" ? t.starting : t.off;
  }

  /** The setup hotspot's name, followed by the sentence end, when the device reports it. */
  function hotspotName() {
    const ap = status?.access_point;
    return ap?.state === "started" && ap.ssid
      ? [" ", mono(ap.ssid, "color: var(--foreground)"), "."]
      : t.end;
  }

  function security(network: VisibleNetwork) {
    if (!network.secured)
      return h("span", { class: "bc-small bc-muted" }, t.open);
    return h(
      "span",
      {
        class: "bc-small bc-muted",
        style: "display: inline-flex; align-items: center; gap: 6px",
      },
      icon(ICON_LOCK, 14),
      t.secured,
    );
  }

  // ---- join forms

  function joinForm(network: VisibleNetwork | null): JoinForm {
    const ssidField = network
      ? null
      : fieldControl({ kind: "text", name: "ssid", label: t.ssid }, lang);
    const passwordLabel = network?.secured ? t.password : t.passwordOpen;
    const passwordField = fieldControl(
      { kind: "secret", name: "password", label: passwordLabel },
      lang,
    );
    const password = passwordField.element.querySelector("input")!;
    password.required = !!network?.secured;
    password.maxLength = 63;
    const ssidInput = ssidField?.element.querySelector("input");
    if (ssidInput) ssidInput.maxLength = 32;
    const submit = button(t.join, lang, { type: "submit" });
    const hint = h("span", { class: "bc-hint" }, t.joinHint);
    const form = h(
      "form",
      { novalidate: true, autocomplete: "off" },
      ssidField?.element,
      passwordField.element,
      submit,
      hint,
    );
    if (phone) {
      form.style.cssText =
        "display: flex; flex-direction: column; gap: 10px; padding: 0 16px 16px";
      for (const input of form.querySelectorAll("input")) {
        input.style.height = "var(--control-lg)";
        input.style.fontSize = "16px";
      }
      for (const control of form.querySelectorAll("button"))
        control.style.height = "var(--control-lg)";
      hint.style.fontSize = "12px";
    } else {
      form.style.cssText = `display: flex; flex-wrap: wrap; align-items: flex-end; gap: var(--space-2); padding: 4px 20px 20px ${network ? 76 : 48}px`;
      ssidField?.element.style.setProperty("flex", "1 1 100%");
      passwordField.element.style.flex = "1 1 260px";
      hint.style.flex = "1 1 100%";
    }

    const validate = () => {
      const ssid = network ? network.ssid : ssidInput!.value;
      const secret = password.value;
      const ssidError = network
        ? null
        : !ssid.trim()
          ? kit.required(t.ssid)
          : bytes(ssid) > 32
            ? t.ssidLength
            : null;
      const passwordError =
        !secret && network?.secured
          ? kit.required(passwordLabel)
          : secret && (bytes(secret) < 8 || bytes(secret) > 63)
            ? t.passwordLength
            : null;
      ssidField?.control.setError(ssidError);
      passwordField.control.setError(passwordError);
      if (ssidError) ssidField!.control.focus();
      else if (passwordError) passwordField.control.focus();
      return ssidError || passwordError ? null : { ssid, password: secret };
    };

    form.addEventListener("submit", (event) => {
      event.preventDefault();
      const body = validate();
      if (body) void join(network ? network.ssid : MANUAL, body);
    });
    return {
      element: form,
      focus: () => (ssidField ?? passwordField).control.focus(),
      clear: () => passwordField.control.clearSecret(),
    };
  }

  function formFor(key: string, network: VisibleNetwork | null) {
    let form = forms.get(key);
    if (!form) {
      form = joinForm(network);
      forms.set(key, form);
    }
    return form;
  }

  function dropForms() {
    for (const form of forms.values()) form.clear();
    forms.clear();
  }

  function toggle(key: string) {
    const opening = picked !== key;
    picked = opening ? key : null;
    renderList();
    if (opening) forms.get(key)?.focus();
  }

  // ---- actions

  async function refresh() {
    const result = await getJson(context, STATUS_URL, STATUS_TIMEOUT_MS);
    if (!result) return;
    if (result.ok && isStatus(result.value)) status = result.value;
    else
      context.toast({
        kind: "error",
        title: t.statusFailed,
        code: !result.ok && result.status ? String(result.status) : undefined,
      });
    update();
  }

  async function scan() {
    if (scanning || !status?.capabilities.scanning) return;
    scanning = true;
    update();
    const result = await getJson(context, SCAN_URL, SCAN_TIMEOUT_MS);
    if (!result) return;
    scanning = false;
    if (result.ok && isScan(result.value))
      networks = nearbyNetworks(result.value);
    else
      context.toast({
        kind: "error",
        title: t.scanFailed,
        code: !result.ok && result.status ? String(result.status) : undefined,
      });
    update();
  }

  async function join(key: string, body: { ssid: string; password: string }) {
    if (busy) return;
    busy = true;
    update();
    const outcome = await submitJson(context, {
      endpoint: STATUS_URL,
      method: "PUT",
      body,
      timeoutMs: JOIN_TIMEOUT_MS,
      success: { title: t.joined(body.ssid), body: t.joinHint },
    });
    if (outcome === "aborted") return;
    busy = false;
    if (outcome === "accepted") {
      forms.get(key)?.clear();
      forms.delete(key);
      picked = null;
    }
    await refresh();
  }

  async function forget() {
    const ssid = status?.station.ssid ?? "—";
    if (busy) return;
    busy = true;
    update();
    const outcome = await submitJson(context, {
      endpoint: STATUS_URL,
      method: "DELETE",
      body: undefined,
      success: { title: t.forgot(ssid) },
    });
    if (outcome === "aborted") return;
    busy = false;
    await refresh();
  }

  // ---- layout

  const figures: HTMLElement[] = [];
  const statusSlot = h("span");
  const networkSlot = h("span");
  const hotspotSlot = h("span");
  const count = h("span", {
    class: "bc-mono bc-muted",
    style: "font-size: 12px",
  });
  const list = h("div");
  const forgetSlot = h("div");
  const rescan = h(
    "button",
    {
      class: "bc-button bc-button--outline bc-button--sm",
      type: "button",
      onclick: () => void scan(),
    },
    icon(ICON_REFRESH),
  );
  const rescanLabel = h("span");
  rescan.append(rescanLabel);

  function desktopLayout() {
    const head = header(
      {
        title: "Wi-Fi",
        lead: t.lead,
        extra: kv(
          [
            [t.status, statusSlot],
            [t.network, networkSlot],
            [t.hotspot, hotspotSlot],
          ],
          lang,
          { live: true },
        ),
        figure: "wifi",
        figureWidth: 300,
      },
      lang,
    );
    head.setAttribute("aria-label", "Wi-Fi");
    const box = head.querySelector<HTMLElement>(".bc-header__figure");
    if (box) box.style.flexBasis = "340px";
    count.style.fontSize = "12px";
    const title = h("h2", { id: "wifi-nearby", class: "bc-title" }, t.nearby);
    const nearby = h(
      "section",
      { class: "bc-frame", "aria-labelledby": "wifi-nearby" },
      h(
        "div",
        {
          style:
            "display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 16px 20px",
        },
        h(
          "div",
          { style: "display: flex; align-items: baseline; gap: 8px" },
          title,
          count,
        ),
        rescan,
      ),
      h(
        "div",
        { class: "bc-thead", style: `grid-template-columns: ${COLUMNS}` },
        h("span", null, t.hSignal),
        h("span", null, t.hNetwork),
        h("span", null, t.hSecurity),
        h("span", { style: "text-align: right" }, t.hStrength),
      ),
      list,
    );
    return page(head, nearby, forgetSlot);
  }

  function phoneLayout() {
    const figure = h("hl-figure", {
      name: "wifi",
      style: "flex: none; width: 150px",
    });
    const statusBox = h(
      "div",
      {
        role: "status",
        style:
          "flex: 1 1 auto; display: flex; flex-direction: column; gap: 6px; min-width: 0",
      },
      h(
        "h1",
        {
          class: "bc-page-title",
          style: "font-size: 26px; line-height: 32px",
        },
        "Wi-Fi",
      ),
      statusSlot,
      networkSlot,
    );
    count.style.fontSize = "";
    return h(
      "div",
      {
        style:
          "display: flex; flex-direction: column; font-size: 15px; line-height: 22px",
      },
      h(
        "section",
        {
          "aria-label": t.phoneCurrent,
          style:
            "padding: var(--space-4); display: flex; align-items: center; gap: 12px; border-bottom: 1px solid var(--border)",
        },
        statusBox,
        figure,
      ),
      h(
        "div",
        {
          style:
            "display: flex; align-items: center; justify-content: space-between; padding: 16px 16px 8px",
        },
        h(
          "h2",
          { class: "bc-list-label", style: "padding: 0" },
          t.nearby,
          " ",
          count,
        ),
        rescan,
      ),
      list,
      forgetSlot,
    );
  }

  function layout() {
    dropForms();
    picked = null;
    const view = phone ? phoneLayout() : desktopLayout();
    figures.splice(
      0,
      figures.length,
      ...view.querySelectorAll<HTMLElement>("hl-figure"),
    );
    root.replaceChildren(view);
    update();
  }

  // ---- updates

  function message(text: string) {
    return h(
      phone ? "li" : "div",
      {
        class: "bc-small bc-muted",
        style: phone
          ? "padding: 14px 16px; border-top: 1px solid var(--border)"
          : "padding: 16px 20px; border-top: 1px solid var(--border)",
      },
      text,
    );
  }

  function desktopRow(network: VisibleNetwork) {
    const current = isCurrent(network);
    const open = picked === network.ssid && !current;
    const cells = [
      bars(signalBars(network.signal_dbm)),
      h(
        "span",
        {
          style: "display: flex; align-items: center; gap: 8px; min-width: 0",
        },
        h(
          "span",
          { style: "font-weight: 500; overflow-wrap: anywhere" },
          network.ssid,
        ),
        current ? badge(t.current, lang) : null,
      ),
      security(network),
      h(
        "span",
        {
          class: "bc-mono bc-muted",
          style: "text-align: right; font-size: 12px",
        },
        network.signal_dbm === null ? "—" : `${network.signal_dbm} dBm`,
      ),
    ];
    const style = `grid-template-columns: ${COLUMNS}`;
    const row =
      current || !canJoin()
        ? h(
            "div",
            { class: "bc-trow", style: `${style}; cursor: default` },
            ...cells,
          )
        : h(
            "button",
            {
              class: "bc-trow",
              type: "button",
              "aria-expanded": String(open),
              style,
              onclick: () => toggle(network.ssid),
            },
            ...cells,
          );
    return h(
      "div",
      {
        style: `background: var(${open ? "--sidebar" : "--background"})`,
      },
      row,
      open ? formFor(network.ssid, network).element : null,
    );
  }

  function phoneRow(network: VisibleNetwork) {
    const current = isCurrent(network);
    const open = picked === network.ssid && !current;
    const cells = [
      bars(signalBars(network.signal_dbm)),
      h(
        "span",
        {
          style: `flex: 1 1 auto; min-width: 0; display: flex; align-items: center; gap: 8px; overflow-wrap: anywhere${open ? "; font-weight: 500" : ""}`,
        },
        network.ssid,
        current ? badge(t.current, lang) : null,
      ),
      security(network),
    ];
    const row =
      current || !canJoin()
        ? h("div", { class: "bc-list-row", style: "cursor: default" }, ...cells)
        : h(
            "button",
            {
              class: "bc-list-row",
              type: "button",
              "aria-expanded": String(open),
              onclick: () => toggle(network.ssid),
            },
            ...cells,
          );
    return h(
      "li",
      { style: open ? "background: var(--background-200)" : undefined },
      row,
      open ? formFor(network.ssid, network).element : null,
    );
  }

  function manualRow() {
    const open = picked === MANUAL;
    const row = h(
      "button",
      {
        class: "bc-list-row",
        type: "button",
        "aria-expanded": String(open),
        style: phone
          ? "font-weight: 500"
          : `padding: 0 20px; font-weight: 500; border-bottom: 0${open ? "" : "; border-radius: 0 0 7px 7px"}`,
        onclick: () => toggle(MANUAL),
      },
      icon(ICON_PLUS, phone ? 18 : undefined),
      h("span", { style: "flex: 1 1 auto" }, t.manual),
      phone ? null : icon(open ? ICON_CHEVRON_DOWN : ICON_CHEVRON_RIGHT),
    );
    return h(
      phone ? "li" : "div",
      {
        style: phone
          ? `border-bottom: 1px solid var(--border)${open ? "; background: var(--background-200)" : ""}`
          : open
            ? "background: var(--sidebar); border-radius: 0 0 7px 7px"
            : undefined,
      },
      row,
      open ? formFor(MANUAL, null).element : null,
    );
  }

  function renderList() {
    const focused = document.activeElement;
    const rows: Node[] = [];
    if (status && !status.capabilities.scanning) rows.push(message(t.managed));
    else if (networks === null) {
      if (scanning) rows.push(message(t.scanning));
    } else if (!networks.length)
      rows.push(message(scanning ? t.scanning : t.none));
    else
      rows.push(
        ...networks.map((network) =>
          phone ? phoneRow(network) : desktopRow(network),
        ),
      );
    if (canJoin()) rows.push(manualRow());
    const body = phone
      ? h("ul", { style: "margin: 0; padding: 0; list-style: none" }, rows)
      : h("div", null, rows);
    list.replaceChildren(body);
    if (focused instanceof HTMLElement && list.contains(focused))
      focused.focus();
    for (const control of list.querySelectorAll<HTMLButtonElement>(
      "form button[type=submit]",
    ))
      control.disabled = busy;
  }

  function renderForget() {
    forgetSlot.replaceChildren();
    if (!canJoin() || status?.station.state !== "connected") return;
    const ssid = status.station.ssid ?? "—";
    const action = button(phone ? t.phoneForget(ssid) : t.forgetBtn, lang, {
      variant: "danger",
      onClick: () => void forget(),
    });
    (action as HTMLButtonElement).disabled = busy;
    if (phone) {
      action.style.height = "var(--control-lg)";
      forgetSlot.style.cssText =
        "padding: var(--space-6) var(--space-4); display: flex; flex-direction: column; gap: 8px";
      forgetSlot.append(
        action,
        h(
          "span",
          { class: "bc-hint", style: "font-size: 12px" },
          t.phoneForgetHint,
          hotspotName(),
        ),
      );
      return;
    }
    forgetSlot.style.cssText = "";
    forgetSlot.append(
      h(
        "section",
        {
          class: "bc-frame",
          "aria-labelledby": "wifi-forget",
          style:
            "display: flex; flex-wrap: wrap; align-items: center; gap: 16px; padding: 20px",
        },
        h(
          "div",
          {
            style:
              "flex: 1 1 320px; display: flex; flex-direction: column; gap: 4px",
          },
          h("h2", { id: "wifi-forget", class: "bc-title" }, t.forget),
          h(
            "span",
            { class: "bc-small bc-muted" },
            t.forgetDesc(ssid),
            hotspotName(),
          ),
        ),
        action,
      ),
    );
  }

  function update() {
    statusSlot.replaceChildren(stateBadge());
    statusSlot.style.alignSelf = phone ? "flex-start" : "";
    const ssid =
      status?.station.state === "connected" ? status.station.ssid : undefined;
    networkSlot.replaceChildren(
      ssid ? mono(ssid, phone ? "font-size: 14px" : "") : phone ? "" : "—",
    );
    hotspotSlot.replaceChildren(hotspotValue());
    count.textContent = networks ? twoDigits(networks.length) : "";
    rescanLabel.textContent = scanning ? t.scanning : t.rescan;
    rescan.setAttribute("aria-busy", String(scanning));
    rescan.disabled = !!status && !status.capabilities.scanning;
    for (const figure of figures) figure.setAttribute("scan", String(scanning));
    renderList();
    renderForget();
  }

  // ---- lifetime

  const onMedia = () => {
    if (!media || media.matches === phone) return;
    phone = media.matches;
    layout();
  };
  media?.addEventListener("change", onMedia);
  const cleanup = () => {
    media?.removeEventListener("change", onMedia);
    dropForms();
    root.replaceChildren();
  };
  signal.addEventListener("abort", cleanup, { once: true });

  layout();
  void refresh().then(() => scan());
  return cleanup;
};
