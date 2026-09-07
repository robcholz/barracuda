import { ModuleSession, parseEntries, type WebEntry } from "./runtime";

function element<T extends HTMLElement>(id: string): T {
  const value = document.getElementById(id);
  if (!value) throw new Error(`Missing element ${id}`);
  return value as T;
}
const nav = element("navigation"),
  cards = element("cards"),
  overview = element("overview");
const panel = element("module-panel"),
  notice = element("notice"),
  empty = element("empty");
const refresh = element<HTMLButtonElement>("refresh"),
  connection = element("connection");
const session = new ModuleSession();
let entries: WebEntry[] = [];
let selected: WebEntry | undefined;
let loading = false;
let navigationVersion = 0;
let requestController: AbortController | undefined;

function message(text: string) {
  notice.textContent = text;
  notice.hidden = !text;
}
function button(text: string, className: string, action: () => void) {
  const node = document.createElement("button");
  node.type = "button";
  node.className = className;
  node.textContent = text;
  node.addEventListener("click", action);
  return node;
}
function renderNavigation() {
  const home = button(
    "◫　概览",
    `nav-item${selected ? "" : " active"}`,
    showHome,
  );
  if (!selected) home.setAttribute("aria-current", "page");
  nav.replaceChildren(home);
  for (const entry of entries) {
    const item = button(
      `◇　${entry.title}`,
      `nav-item${selected?.id === entry.id ? " active" : ""}`,
      () => void openModule(entry),
    );
    if (selected?.id === entry.id) item.setAttribute("aria-current", "page");
    nav.append(item);
  }
}
function renderCards() {
  cards.replaceChildren();
  element("count").textContent = String(entries.length);
  empty.hidden = entries.length > 0;
  element("empty-title").textContent = "还没有启用的网页模块";
  element("empty-description").textContent =
    "模块注册后会自动出现在这里，也可以点击上方刷新。";
  for (const entry of entries) {
    const card = button("", "card", () => void openModule(entry));
    const top = document.createElement("div");
    top.className = "card-top";
    const icon = document.createElement("span");
    icon.className = "module-icon";
    icon.textContent = entry.id.slice(0, 2).toUpperCase();
    const arrow = document.createElement("span");
    arrow.className = "card-arrow";
    arrow.textContent = "↗";
    arrow.setAttribute("aria-hidden", "true");
    const title = document.createElement("h3");
    title.textContent = entry.title;
    const id = document.createElement("p");
    id.textContent = entry.id;
    top.append(icon, arrow);
    card.append(top, title, id);
    cards.append(card);
  }
}
function showHome() {
  navigationVersion++;
  session.close();
  selected = undefined;
  panel.replaceChildren();
  panel.hidden = true;
  overview.hidden = false;
  panel.removeAttribute("aria-busy");
  element("page-title").textContent = "设备工作台";
  element("breadcrumb").textContent = "概览";
  element("subtitle").textContent = "在一个地方，访问设备的全部已启用模块。";
  renderNavigation();
}
async function openModule(entry: WebEntry) {
  const version = ++navigationVersion;
  selected = entry;
  session.close();
  message("");
  overview.hidden = true;
  panel.hidden = false;
  element("page-title").textContent = entry.title;
  element("breadcrumb").textContent = entry.title;
  element("subtitle").textContent = entry.id;
  renderNavigation();
  const root = document.createElement("div");
  const pending = document.createElement("p");
  pending.className = "module-loading";
  pending.textContent = "正在加载模块…";
  panel.replaceChildren(pending, root);
  panel.setAttribute("aria-busy", "true");
  try {
    await session.open(entry, root);
    if (version === navigationVersion) pending.remove();
  } catch (error) {
    if (version !== navigationVersion) return;
    session.close();
    root.remove();
    pending.textContent = "模块加载失败，请重试。";
    panel.append(button("重试加载", "button", () => void openModule(entry)));
    console.error("Module load failed", error);
  } finally {
    if (version === navigationVersion) panel.removeAttribute("aria-busy");
  }
}
async function refreshEntries() {
  if (loading) return;
  loading = true;
  refresh.disabled = true;
  requestController = new AbortController();
  const timeout = setTimeout(() => requestController?.abort(), 8000);
  try {
    const response = await fetch("/portal/entries.json", {
      cache: "no-store",
      signal: requestController.signal,
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const next = parseEntries(await response.json());
    entries = next;
    const current = selected && next.find((entry) => entry.id === selected?.id);
    if (selected && !current) {
      showHome();
      message("当前模块已停用，已返回概览。");
    } else if (
      selected &&
      current &&
      (current.module !== selected.module || current.title !== selected.title)
    ) {
      void openModule(current);
    } else message("");
    renderNavigation();
    renderCards();
    connection.textContent = "设备已连接";
    connection.dataset.state = "online";
  } catch (error) {
    connection.textContent = "连接未就绪";
    connection.dataset.state = "error";
    message("无法更新模块清单，请检查与设备的连接后重试。");
    if (!entries.length) {
      element("empty-title").textContent = "暂时无法读取模块";
      element("empty-description").textContent = "点击“刷新模块”重新连接。";
    }
    console.error("Manifest refresh failed", error);
  } finally {
    clearTimeout(timeout);
    loading = false;
    refresh.disabled = false;
  }
}
refresh.addEventListener("click", () => void refreshEntries());
document.querySelector(".brand")?.addEventListener("click", showHome);
document.querySelector("[data-home]")?.addEventListener("click", showHome);
document.addEventListener("visibilitychange", () => {
  if (!document.hidden) void refreshEntries();
});
const startPolling = () =>
  setInterval(() => {
    if (!document.hidden) void refreshEntries();
  }, 30_000);
let poll = startPolling();
window.addEventListener("pagehide", () => {
  clearInterval(poll);
  requestController?.abort();
  session.close();
});
window.addEventListener("pageshow", (event) => {
  if (event.persisted) {
    showHome();
    poll = startPolling();
    void refreshEntries();
  }
});
void refreshEntries();
