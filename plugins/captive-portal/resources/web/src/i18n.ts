import type { Lang, WebGroup } from "./contract";

/**
 * The shell's copy, from the design's string tables (gen_boards.py: SIDEBAR_T, TOPBAR_T, OV_T,
 * STATUS_T, APP_T, MOB_T). Entry titles and summaries come from the manifest, never from here.
 */
const zh = {
  portal: "设备门户",
  overview: "概览",
  groups: { device: "设备", agent: "智能体", channel: "消息通道" },
  footer: "HTTP 明文连接",
  footerTip: "仅在可信网络中提交密钥",
  collapse: "收起侧栏",
  expand: "展开侧栏",
  language: "界面语言",
  theme: "主题",
  light: "浅色",
  dark: "深色",
  system: "跟随系统",
  langButton: "中文",
  themeLabel: {
    system: "主题：跟随系统",
    light: "主题：浅色",
    dark: "主题：深色",
  },
  offline: "连接未就绪",
  close: "关闭",
  lead: "配置这台设备的网络、模型与消息通道。",
  phoneLead: "配置网络、模型与消息通道。",
  figAlt: "Barracuda 开发板：射频模组、USB-C 接口与两排排针",
  kvPages: "插件页面",
  pagesTip: "每个页面由一个插件提供，停用插件后页面随之消失",
  kvConn: "连接",
  plain: "明文",
  plainTip: "仅在可信网络中提交密钥",
  start: "开始使用",
  steps: {
    device: "连接 Wi-Fi",
    agent: "注册模型",
    channel: "接入消息通道",
  },
  open: (title: string) => joinZh("前往", title),
  tryFirst: (title: string) => joinZh(joinZh("先用", title), "试试"),
  modules: "模块",
  channels: "消息通道",
  empty: {
    title: "还没有启用的网页模块",
    body: "插件注册的网页模块会出现在这里。",
  },
  unavailable: {
    title: (name: string) => joinZh(name, "模块已停用"),
    body: (name: string) =>
      joinZh(joinZh("在设备上启用", name), "插件后刷新。"),
  },
  loading: "正在加载模块…",
  failed: { title: "模块加载失败", body: "检查与设备的连接后重试。" },
  manifestFailed: {
    title: "无法读取模块清单",
    body: "检查与设备的连接后重试。",
  },
  refresh: "刷新模块",
  back: "返回概览",
  retry: "重试加载",
};

type Strings = typeof zh;

const en: Strings = {
  portal: "Device portal",
  overview: "Overview",
  groups: { device: "Device", agent: "Agent", channel: "Channels" },
  footer: "Plain HTTP",
  footerTip: "Submit keys only on a trusted network",
  collapse: "Collapse sidebar",
  expand: "Expand sidebar",
  language: "Language",
  theme: "Theme",
  light: "Light",
  dark: "Dark",
  system: "System",
  langButton: "EN",
  themeLabel: {
    system: "Theme: system",
    light: "Theme: light",
    dark: "Theme: dark",
  },
  offline: "Not connected",
  close: "Dismiss",
  lead: "Set up this device's network, models and message channels.",
  phoneLead: "Set up the network, models and message channels.",
  figAlt: "Barracuda board: radio module, USB-C port and two pin headers",
  kvPages: "Plugin pages",
  pagesTip:
    "Each page comes from a plugin and goes away when the plugin is turned off",
  kvConn: "Connection",
  plain: "plain text",
  plainTip: "Submit keys only on a trusted network",
  start: "Get started",
  steps: {
    device: "Connect Wi-Fi",
    agent: "Register a model",
    channel: "Connect a channel",
  },
  open: (title: string) => `Open ${title}`,
  tryFirst: (title: string) => `Try ${title} first`,
  modules: "Modules",
  channels: "Message channels",
  empty: {
    title: "No web modules yet",
    body: "Web modules that plugins register show up here.",
  },
  unavailable: {
    title: (name: string) => `The ${name} module is turned off`,
    body: (name: string) =>
      `Turn the ${name} plugin on on the device, then refresh.`,
  },
  loading: "Loading module…",
  failed: {
    title: "The module didn't load",
    body: "Check the connection to the device and try again.",
  },
  manifestFailed: {
    title: "Couldn't read the module list",
    body: "Check the connection to the device and try again.",
  },
  refresh: "Refresh modules",
  back: "Back to overview",
  retry: "Retry",
};

export const STRINGS: Record<Lang, Strings> = { zh, en };

export type GroupLabels = Record<WebGroup, string>;

/** Joins Chinese and Latin text the way the design's copy does: a space only where CJK meets Latin. */
export function joinZh(left: string, right: string): string {
  const cjk = /[　-鿿＀-￯]/;
  const a = left.slice(-1),
    b = right.slice(0, 1);
  return a && b && cjk.test(a) !== cjk.test(b) && !/\s/.test(a + b)
    ? `${left} ${right}`
    : left + right;
}
