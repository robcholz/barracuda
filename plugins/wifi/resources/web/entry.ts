import type { PortalModule } from "../../../captive-portal/resources/web/src/runtime";
import { element, panel } from "../../../captive-portal/resources/web/ui/form";

interface WifiStatus {
  capabilities: {
    access_point: boolean;
    scanning: boolean;
    station_configuration: boolean;
  };
  station: { state: string; ssid?: string };
  access_point: { state: string; ssid?: string };
}

interface VisibleNetwork {
  ssid: string;
  signal_dbm: number | null;
  secured: boolean;
}

function statusText(status: WifiStatus) {
  if (!status.capabilities.station_configuration)
    return status.station.state === "connected"
      ? "网络由当前平台管理；Barracuda 已可使用网络。"
      : "当前平台尚未提供可控制的 Wi-Fi，网络未连接。";
  if (status.station.state === "connected")
    return `已连接${status.station.ssid ? `：${status.station.ssid}` : ""}`;
  if (status.station.state === "connecting") return "正在连接…";
  if (status.access_point.state === "started")
    return `尚未连接；配置热点 ${status.access_point.ssid ?? "Barracuda Setup"} 已开启。`;
  return "尚未连接。";
}

export const mount: PortalModule["mount"] = (root, { signal }) => {
  if (signal.aborted) return;
  const page = panel(
    root,
    "Wi-Fi",
    "扫描、连接或忘记无线网络。凭据只保存到设备的 Wi-Fi Plugin 存储。",
  );
  const current = element("p", "正在读取网络状态…");
  current.setAttribute("role", "status");
  current.setAttribute("aria-live", "polite");
  const form = element("form");
  form.autocomplete = "off";
  const fields = element("fieldset");
  const ssidLabel = element("label", "网络名称（SSID）");
  const ssid = element("input");
  ssid.name = "ssid";
  ssid.required = true;
  ssid.maxLength = 32;
  ssid.autocomplete = "off";
  const choices = element("datalist");
  choices.id = "wifi-networks";
  ssid.setAttribute("list", choices.id);
  ssidLabel.append(ssid, choices);
  const passwordLabel = element("label", "密码（开放网络留空）");
  const password = element("input");
  password.name = "password";
  password.type = "password";
  password.maxLength = 63;
  password.autocomplete = "new-password";
  passwordLabel.append(password);
  const actions = element("div");
  const scan = element("button", "扫描网络");
  scan.type = "button";
  const connect = element("button", "连接");
  connect.type = "submit";
  const forget = element("button", "忘记网络并开启配置热点");
  forget.type = "button";
  actions.append(scan, connect, forget);
  fields.append(element("legend", "Station 连接"), ssidLabel, passwordLabel);
  form.append(fields, actions);
  page.append(current, form);

  const lifecycle = new AbortController();
  const cleanup = () => {
    lifecycle.abort();
    password.value = "";
    page.remove();
    signal.removeEventListener("abort", cleanup);
  };
  signal.addEventListener("abort", cleanup, { once: true });

  const refresh = async () => {
    try {
      const response = await fetch("/api/wifi", {
        cache: "no-store",
        signal: lifecycle.signal,
      });
      if (!response.ok) throw new Error(String(response.status));
      const status = (await response.json()) as WifiStatus;
      current.textContent = statusText(status);
      fields.disabled = !status.capabilities.station_configuration;
      connect.disabled = !status.capabilities.station_configuration;
      forget.disabled = !status.capabilities.station_configuration;
      scan.disabled = !status.capabilities.scanning;
    } catch {
      if (!lifecycle.signal.aborted)
        current.textContent = "无法读取 Wi-Fi 状态。";
    }
  };

  scan.addEventListener(
    "click",
    async () => {
      scan.disabled = true;
      current.textContent = "正在扫描网络…";
      try {
        const response = await fetch("/api/wifi/scan", {
          cache: "no-store",
          signal: lifecycle.signal,
        });
        if (!response.ok) throw new Error(String(response.status));
        const networks = (await response.json()) as VisibleNetwork[];
        choices.replaceChildren(
          ...networks
            .filter((network) => network.ssid)
            .sort(
              (left, right) =>
                (right.signal_dbm ?? -128) - (left.signal_dbm ?? -128),
            )
            .map((network) => {
              const option = element("option");
              option.value = network.ssid;
              option.label = `${network.secured ? "🔒 " : ""}${network.signal_dbm ?? "?"} dBm`;
              return option;
            }),
        );
        current.textContent = `发现 ${networks.length} 个网络。`;
        ssid.focus();
      } catch {
        if (!lifecycle.signal.aborted)
          current.textContent = "扫描失败，请重试。";
      } finally {
        scan.disabled = false;
      }
    },
    { signal: lifecycle.signal },
  );

  form.addEventListener(
    "submit",
    async (event) => {
      event.preventDefault();
      password.setCustomValidity("");
      if (password.value && password.value.length < 8)
        password.setCustomValidity("密码应为空或至少 8 个字符");
      if (!form.reportValidity()) return;
      fields.disabled = true;
      connect.disabled = true;
      current.textContent = `正在连接 ${ssid.value}…`;
      try {
        const response = await fetch("/api/wifi", {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ ssid: ssid.value, password: password.value }),
          signal: lifecycle.signal,
          redirect: "error",
        });
        current.textContent =
          response.status === 204
            ? "连接成功，配置热点即将关闭。"
            : response.status === 422
              ? "无法连接，请检查网络名称和密码。"
              : `连接失败（HTTP ${response.status}）。`;
        if (response.status === 204) password.value = "";
      } catch {
        if (!lifecycle.signal.aborted)
          current.textContent =
            "连接切换期间失去页面连接；请检查设备是否已加入目标网络。";
      } finally {
        fields.disabled = false;
        connect.disabled = false;
      }
    },
    { signal: lifecycle.signal },
  );

  forget.addEventListener(
    "click",
    async () => {
      forget.disabled = true;
      current.textContent = "正在忘记网络…";
      try {
        const response = await fetch("/api/wifi", {
          method: "DELETE",
          signal: lifecycle.signal,
        });
        current.textContent = response.ok
          ? "已忘记网络，配置热点正在启动。"
          : `操作失败（HTTP ${response.status}）。`;
      } catch {
        if (!lifecycle.signal.aborted)
          current.textContent = "操作失败，请重试。";
      } finally {
        forget.disabled = false;
      }
    },
    { signal: lifecycle.signal },
  );

  void refresh();
  return cleanup;
};
