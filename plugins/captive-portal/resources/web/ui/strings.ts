import type { Lang } from "../src/contract";

/** The kit's own copy (the design's FORM_T and APP_T toasts). Pages bring their own strings. */
export const KIT_STRINGS: Record<
  Lang,
  {
    advanced: string;
    advancedHint: string;
    show: string;
    hide: string;
    clear: string;
    optional: string;
    required: (label: string) => string;
    url: string;
    number: (min: number, max: number) => string;
    accepted: string;
    rejected: string;
    missing: string;
    failed: string;
    unreachable: string;
    unreachableHint: string;
    noReply: string;
    retry: string;
    channel: string;
    configured: string;
  }
> = {
  zh: {
    advanced: "高级",
    advancedHint: "已填入默认值",
    show: "显示",
    hide: "隐藏",
    clear: "清空",
    optional: "可选",
    // a space separates Chinese from a Latin label (请填写 API Key), not from a Chinese one (请填写密码)
    required: (label) =>
      `请填写${/^[\x21-\x7e]/.test(label) ? " " : ""}${label}。`,
    url: "请输入以 http:// 或 https:// 开头的地址。",
    number: (min, max) => `请输入 ${min} 到 ${max} 之间的整数。`,
    accepted: "设备已接受配置",
    rejected: "配置被拒绝",
    missing: "接口不可用",
    failed: "提交失败",
    unreachable: "设备连不上服务",
    unreachableHint: "检查设备的网络后重试。",
    noReply: "未收到设备确认",
    retry: "重试",
    channel: "通道",
    configured: "已配置",
  },
  en: {
    advanced: "Advanced",
    advancedHint: "Defaults filled in",
    show: "Show",
    hide: "Hide",
    clear: "Clear",
    optional: "optional",
    required: (label) => `Enter the ${label}.`,
    url: "Enter an address starting with http:// or https://.",
    number: (min, max) => `Enter a whole number from ${min} to ${max}.`,
    accepted: "The device accepted the configuration",
    rejected: "Configuration rejected",
    missing: "Endpoint not available",
    failed: "Submission failed",
    unreachable: "The device can't reach the service",
    unreachableHint: "Check the device's network, then try again.",
    noReply: "No confirmation from the device",
    retry: "Retry",
    channel: "Channel",
    configured: "Configured",
  },
};
