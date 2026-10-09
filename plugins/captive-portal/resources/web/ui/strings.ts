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
    noReply: string;
    noReplyBody: string;
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
    required: (label) => `请填写 ${label}。`,
    url: "请输入以 http:// 或 https:// 开头的地址。",
    number: (min, max) => `请输入 ${min} 到 ${max} 之间的整数。`,
    accepted: "设备已接受配置",
    rejected: "配置被拒绝",
    missing: "接口不可用",
    failed: "提交失败",
    noReply: "未收到设备确认",
    noReplyBody: "配置可能已生效。",
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
    noReply: "No confirmation from the device",
    noReplyBody: "The settings may already be applied.",
    retry: "Retry",
    channel: "Channel",
    configured: "Configured",
  },
};
