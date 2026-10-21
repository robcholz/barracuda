/**
 * Live Editor's words, in English and Simplified Chinese. Pages read `t` at render time; a change
 * of language renders every page again. What goes to the model (tool results, the notes the chat
 * sends it) stays in English, and so do the titles the editor stores: `stored` shows those in the
 * reader's language.
 */

export type Lang = "en" | "zh";

const en = {
  /** The language's own name, for the language menu. */
  name: "English",
  /** The `lang` attribute of the page. */
  tag: "en",
  language: "Language",
  theme: "Theme",
  light: "Light",
  dark: "Dark",
  system: "System",
  close: "Close",
  cancel: "Cancel",
  apps: "Apps",
  settings: "Settings",
  website: "Barracuda website",

  // The sidebar and the shell
  newApp: "New app",
  newChat: "New chat",
  chatsOf: (name: string) => `Chats · ${name}`,
  device: "Device",
  expandSidebar: "Expand sidebar",
  collapseSidebar: "Collapse sidebar",

  // dom.ts
  justNow: "just now",
  minAgo: (n: number) => `${n} min ago`,
  hoursAgo: (n: number) => `${n} h ago`,

  // New app
  ideas: ["Show the room temperature", "A kitchen timer with big digits"],
  describePlaceholder: "A kitchen timer: big minutes and seconds, tap to start",
  describeLabel: "Describe the app",
  startFromExample: "Start from an example",
  examples: {
    counter: { title: "Counter", text: "Buttons, a list, saved state" },
    air: { title: "Air quality", text: "Readings from a sensor" },
    lamp: { title: "Light strip", text: "A color picker for a light strip" },
    camera: { title: "Camera", text: "Take and show a picture" },
  },
  newAppLead:
    "Say what the device should do. Barracuda writes the screen and the program, then runs them in the preview.",
  createFailed: "The app could not be created",
  addAttachment: "Add attachment",
  removeAttachment: "Remove attachment",
  send: "Send",
  stop: "Stop",
  previewDevice: "Preview device",
  yourApps: "Your apps",

  // The workspace
  views: {
    preview: "Preview",
    code: "Code",
    changes: "Changes",
    console: "Console",
  },
  viewsLabel: "Views",
  toReview: (label: string, n: number) => `${label} · ${n} to review`,
  errorCount: (label: string, n: number) =>
    `${label} · ${n} error${n === 1 ? "" : "s"}`,
  resizeViews: "Resize the views",
  built: "Built",
  buildFailed: "Build failed",
  building: "Building",
  versions: "Versions",
  restoringKeeps: "Restoring keeps the newer versions",
  install: "Install",
  installOnDevice: "Install on device",

  // Chat
  chat: "Chat",
  message: "Message",
  askBarracuda: "Ask Barracuda",
  replyToBarracuda: "Reply to Barracuda",
  cancelReply: "Cancel reply",
  chatEmpty:
    "Say what to change. Barracuda edits the code, builds it and checks it in the preview.",
  sent: "Sent",
  thinking: "Thinking",
  working: "Working",
  stopped: "Stopped",
  serviceFailed: "The model service failed",
  copy: "Copy",
  copied: "Copied",
  reply: "Reply",
  thoughtFor: (seconds: Node) => ["Thought for ", seconds, " s"],
  running: "Running",
  failed: "Failed",
  succeeded: "Succeeded",
  lines: (n: number) => `${n} lines`,
  applied: "Applied",
  discarded: "Discarded",
  waitingForReview: "Waiting for review",
  filesChanged: (n: number) => `${n} file${n === 1 ? "" : "s"} changed`,
  reviewChanges: "Review changes",
  openCode: "Open code",
  tooLarge: (name: string) => `${name} is larger than 512 KB`,
  tools: {
    list_files: "List files",
    read_file: "Read",
    write_file: "Write",
    edit_file: "Edit",
    delete_file: "Delete",
    build: "Build",
    look: "Look at the preview",
    tap: "Tap the preview",
    read_console: "Read the console",
    restart: "Restart the app",
  } as Record<string, string>,

  // Preview
  lastBuild: "Last build",
  badgeRunning: "Running",
  background: "Background",
  starting: "Starting",
  ended: "Ended",
  versionLabel: "Version",
  before: "Before",
  after: "After",
  restartApp: "Restart app",
  closeView: (view: string) => `Close ${view}`,
  runningLastBuild: " · running the last build",
  showProblem: "Show problem",
  needsDisplay: "Needs a display",
  needsDisplayText: (name: string) =>
    `${name} has a view. Pick a device with a display to see it.`,
  useRound: "Use Round 360",
  appEnded: (name: string) => `${name} ended`,
  runsHeadless: (name: string) => `${name} runs headless`,
  headlessText: "It has no view. Its output is in the Console.",
  openConsole: "Open Console",
  onPreviewDevice: (name: string) => `${name} on the preview device`,
  inBackground: (name: string) => `${name} is in the background`,
  open: "Open",
  start: "Start",
  deny: "Deny",
  allow: "Allow",
  back: "Back",
  terminate: "Terminate",
  resetStorage: "Reset storage",
  devices: {
    round: "Round 360 · touch",
    square: "Square 320 · touch",
    none: "No display",
  },
  headless: "headless",

  // Changes
  files: (n: number) => ` file${n === 1 ? "" : "s"} · `,
  discard: "Discard",
  apply: "Apply",
  nothingToReview: "Nothing to review",
  nothingToReviewText:
    "Changes Barracuda proposes wait here until you apply them.",
  deleted: " (deleted)",
  added: " (new)",

  // Console
  levels: { all: "All", warn: "Warnings", error: "Errors" },
  level: "Level",
  clearConsole: "Clear console",
  consoleOutput: "Console output",
  copyOutput: "Copy output",
  noOutput: "No console output.",

  // Code
  codeOf: (name: string) => `Code of ${name}`,
  fileLabel: (name: string) => `File: ${name}`,
  filesLabel: "Files",
  oneProblem: "1 problem",
  noProblems: "No problems",
  findInFile: "Find in file",
  moreActions: "More actions",
  fileActions: "File actions",
  newFile: "New file",
  rename: "Rename",
  deleteFile: "Delete file",
  needs: (name: string, file: Node) => [`${name} needs `, file],
  deleteTitle: "Delete this file?",
  deleteText: (file: Node) => [file, " leaves the app. Versions keep a copy."],
  fileName: "File name",
  newName: "New name",
  addFile: "Add file",
  previousMatch: "Previous match",
  nextMatch: "Next match",
  problem: "Problem",
  askToFix: "Ask Barracuda to fix",
  fixTheBuild: (message: string) => `Fix the build: ${message}`,
  matchOf: (index: number, count: number) => `${index} of ${count}`,
  noMatches: "No matches",

  // Settings
  settingsLead:
    "The model Barracuda writes apps with, how its changes reach your code, and the open app's manifest.",
  anthropicHint: "Your Anthropic API key",
  openaiTitle: "OpenAI",
  openaiHint: "Any service with the same API",
  urlError: "Enter the service's base URL, like https://api.example.com/v1",
  chars: (n: number) => `Use 1–${n} characters`,
  settingsSaved: "Settings saved",
  discardEdits: "Discard edits",
  saveSettings: "Save settings",
  modelService: "Model service",
  modelServiceText: "Where the editor's agent runs",
  baseUrl: "API Base URL",
  apiKey: "API Key",
  pasteKey: "Paste your key",
  hide: "Hide",
  show: "Show",
  keptHere: "Kept in this browser",
  sentOnlyTo: "Sent only to the model service",
  model: "Model",
  modelId: "Model ID",
  changes: "Changes",
  changesText: "How Barracuda's edits reach your code",
  review: "Review before applying",
  reviewText: "Proposed changes wait in Changes until you apply them",
  tap: "Let Barracuda tap the preview",
  tapText: "It checks its own changes on the preview device",
  manifest: "The open app's manifest",
  appName: "App name",
  appId: "App ID",
  fixed: "Fixed",
  fixedText: "Devices know the app by its ID",
  version: "Version",
  shape: "Display shape",
  round: "Round",
  rectangle: "Rectangle",
  roundText: "Keeps to the inscribed circle, so it fits every display",
  rectText: "Uses the corners, so it needs a rectangular display",

  // Install
  blocked: {
    title: "The portal window did not open",
    text: "Allow pop-ups for this site, then open the portal again.",
  },
  silent: {
    title: "The portal did not answer",
    text: "Check the address, and that this computer is on the same network as the device.",
  },
  refused: {
    title: (name: string) => `${name} was not installed`,
    text: "It was not installed from the portal window. Open the portal again to install it.",
  },
  deviceFailed: "The device could not write the app",
  doesNotBuild: (message: string) => `It does not build: ${message}`,
  addressError: "Enter the device's address, like 192.168.1.42:8787",
  downloaded: (folder: string) => `${folder}.zip downloaded`,
  downloadedText: (folder: string) =>
    `Unzip it, then in the device portal under Files make the folder media/apps/${folder} and upload its files there.`,
  portalOpen: "Portal open",
  portalClosed: "Portal closed",
  installLead: (name: string) =>
    `${name} goes to the device through its portal, in a window next to this one. You install it from there.`,
  steps: (name: string) => [
    "Open the device portal",
    `Install ${name} from the portal window`,
    "The device writes it with its apps",
  ],
  deviceText: "A Barracuda on this computer's network",
  deviceAddress: "Device address",
  addressHint: "The address the device portal opens at, like ",
  waitingForPortal: "Waiting for the portal window",
  portal: "Portal",
  app: "App",
  builtHere: "Built in this browser",
  buildingApp: (name: string) => `Building ${name}`,
  cannotInstall: (name: string) => `${name} cannot be installed`,
  folder: "Folder",
  installText: "You choose Install in the portal window",
  chooseInstall:
    'The portal window shows what it received. Choose "Install on device" there.',
  writing: "Writing ",
  onDevice: (name: string) => `${name} is on the device`,
  openFromLauncher: "Open it from the device's launcher.",
  download: (folder: string) => `Download ${folder}.zip`,
  backTo: (name: string) => `Back to ${name}`,
  openPortal: "Open device portal",
  openPortalAgain: "Open the portal again",

  // The app's notices
  restored: (n: number, title: string) => `Restored v${n} · ${title}`,
  changesApplied: "Changes applied",
  storageReset: "Storage reset",
  noAppOpen: "No app is open",
  badFileName: "Use 1–64 letters, digits, dots, dashes or underscores",
  alreadyFile: (name: string) => `${name} is already a file`,

  // The agent's failures, as the chat shows them
  needsAnthropicKey: "Add your Anthropic API key in Settings to start.",
  needsOpenai:
    "Add the model service's base URL and model in Settings to start.",
  declined: (detail?: string) =>
    `The model declined this request${detail ? `: ${detail}` : "."}`,
  truncated:
    "The response was cut off at its length limit. Ask for a smaller step.",
  anthropic: {
    auth: "Anthropic did not accept the API key. Check it in Settings.",
    permission:
      "This API key may not use the model. Check the model in Settings.",
    notFound: (model: string) =>
      `Anthropic does not know the model ${model}. Check it in Settings.`,
    rateLimit:
      "Anthropic is rate limiting this key. Wait a moment and send again.",
    badRequest: (message: string) =>
      `Anthropic refused the request: ${message}`,
    connection: "Could not reach Anthropic. Check the connection.",
    failed: (status: unknown, message: string) =>
      `Anthropic failed (${status}): ${message}`,
  },
  openai: {
    auth: "The model service did not accept the API key. Check it in Settings.",
    notFound: (url: string, model: string) =>
      `The model service has no ${url}, or no model ${model}. Check Settings.`,
    rateLimit:
      "The model service is rate limiting this key. Wait a moment and send again.",
    failed: (status: unknown, message: string) =>
      `The model service failed (${status}): ${message}`,
    connection:
      "Could not reach the model service. Check the base URL, and that it allows requests from this page.",
  },

  /** A title or note the editor stored, in the reader's language. */
  stored: (text: string) => text,
};

export type Strings = typeof en;

/** The stored titles and notes, which stay in English, in Chinese. */
function storedZh(text: string): string {
  const fixed: Record<string, string> = {
    "New chat": "新对话",
    "First version": "第一个版本",
    "Your edits": "你的修改",
  };
  if (text in fixed) return fixed[text];
  let match = /^You edited (.+)$/.exec(text);
  if (match) return `你编辑了 ${match[1]}`;
  match = /^Before deleting (.+)$/.exec(text);
  if (match) return `删除 ${match[1]} 之前`;
  return text;
}

const zh: Strings = {
  name: "简体中文",
  tag: "zh-Hans",
  language: "界面语言",
  theme: "主题",
  light: "浅色",
  dark: "深色",
  system: "跟随系统",
  close: "关闭",
  cancel: "取消",
  apps: "应用",
  settings: "设置",
  website: "Barracuda 官网",

  newApp: "新建应用",
  newChat: "新对话",
  chatsOf: (name) => `对话 · ${name}`,
  device: "设备",
  expandSidebar: "展开侧栏",
  collapseSidebar: "收起侧栏",

  justNow: "刚刚",
  minAgo: (n) => `${n} 分钟前`,
  hoursAgo: (n) => `${n} 小时前`,

  ideas: ["显示室温", "大数字的厨房计时器"],
  describePlaceholder: "厨房计时器：大号的分和秒，点按开始",
  describeLabel: "描述应用",
  startFromExample: "从示例开始",
  examples: {
    counter: { title: "计数器", text: "按钮、列表和保存的状态" },
    air: { title: "空气质量", text: "来自传感器的读数" },
    lamp: { title: "灯带", text: "灯带的取色器" },
    camera: { title: "相机", text: "拍一张照片并显示它" },
  },
  newAppLead:
    "说出设备要做什么。Barracuda 编写屏幕和程序，然后在预览里运行它们。",
  createFailed: "无法创建应用",
  addAttachment: "添加附件",
  removeAttachment: "移除附件",
  send: "发送",
  stop: "停止",
  previewDevice: "预览设备",
  yourApps: "你的应用",

  views: {
    preview: "预览",
    code: "代码",
    changes: "修改",
    console: "控制台",
  },
  viewsLabel: "视图",
  toReview: (label, n) => `${label} · ${n} 个待审阅`,
  errorCount: (label, n) => `${label} · ${n} 个错误`,
  resizeViews: "调整视图宽度",
  built: "已构建",
  buildFailed: "构建失败",
  building: "正在构建",
  versions: "版本",
  restoringKeeps: "恢复后，较新的版本仍会保留",
  install: "安装",
  installOnDevice: "安装到设备",

  chat: "对话",
  message: "消息",
  askBarracuda: "问 Barracuda",
  replyToBarracuda: "回复 Barracuda",
  cancelReply: "取消回复",
  chatEmpty: "说出要修改什么。Barracuda 会修改代码、构建，并在预览里检查。",
  sent: "已发送",
  thinking: "正在思考",
  working: "正在处理",
  stopped: "已停止",
  serviceFailed: "模型服务出错",
  copy: "复制",
  copied: "已复制",
  reply: "回复",
  thoughtFor: (seconds) => ["思考了 ", seconds, " 秒"],
  running: "运行中",
  failed: "失败",
  succeeded: "成功",
  lines: (n) => `${n} 行`,
  applied: "已应用",
  discarded: "已丢弃",
  waitingForReview: "等待审阅",
  filesChanged: (n) => `修改了 ${n} 个文件`,
  reviewChanges: "审阅修改",
  openCode: "打开代码",
  tooLarge: (name) => `${name} 超过 512 KB`,
  tools: {
    list_files: "列出文件",
    read_file: "读取",
    write_file: "写入",
    edit_file: "编辑",
    delete_file: "删除",
    build: "构建",
    look: "查看预览",
    tap: "点按预览",
    read_console: "读取控制台",
    restart: "重启应用",
  },

  lastBuild: "上一次构建",
  badgeRunning: "运行中",
  background: "后台",
  starting: "正在启动",
  ended: "已结束",
  versionLabel: "版本",
  before: "修改前",
  after: "修改后",
  restartApp: "重启应用",
  closeView: (view) => `关闭${view}`,
  runningLastBuild: " · 正在运行上一次构建",
  showProblem: "查看问题",
  needsDisplay: "需要屏幕",
  needsDisplayText: (name) => `${name} 有视图。选择一台有屏幕的设备来查看它。`,
  useRound: "使用 Round 360",
  appEnded: (name) => `${name} 已结束`,
  runsHeadless: (name) => `${name} 无屏幕运行`,
  headlessText: "它没有视图，输出在控制台里。",
  openConsole: "打开控制台",
  onPreviewDevice: (name) => `预览设备上的 ${name}`,
  inBackground: (name) => `${name} 在后台`,
  open: "打开",
  start: "启动",
  deny: "拒绝",
  allow: "允许",
  back: "返回",
  terminate: "终止",
  resetStorage: "清空存储",
  devices: {
    round: "圆形 360 · 触摸",
    square: "方形 320 · 触摸",
    none: "无屏幕",
  },
  headless: "无屏幕",

  files: () => " 个文件 · ",
  discard: "丢弃",
  apply: "应用",
  nothingToReview: "没有待审阅的修改",
  nothingToReviewText: "Barracuda 提议的修改会在这里等你应用。",
  deleted: "（已删除）",
  added: "（新文件）",

  levels: { all: "全部", warn: "警告", error: "错误" },
  level: "级别",
  clearConsole: "清空控制台",
  consoleOutput: "控制台输出",
  copyOutput: "复制输出",
  noOutput: "没有控制台输出。",

  codeOf: (name) => `${name} 的代码`,
  fileLabel: (name) => `文件：${name}`,
  filesLabel: "文件",
  oneProblem: "1 个问题",
  noProblems: "没有问题",
  findInFile: "在文件中查找",
  moreActions: "更多操作",
  fileActions: "文件操作",
  newFile: "新建文件",
  rename: "重命名",
  deleteFile: "删除文件",
  needs: (name, file) => [`${name} 需要 `, file],
  deleteTitle: "删除这个文件？",
  deleteText: (file) => [file, " 将从应用中移除。版本里保留着它的副本。"],
  fileName: "文件名",
  newName: "新名称",
  addFile: "添加文件",
  previousMatch: "上一个匹配",
  nextMatch: "下一个匹配",
  problem: "问题",
  askToFix: "让 Barracuda 修复",
  fixTheBuild: (message) => `修复构建：${message}`,
  matchOf: (index, count) => `第 ${index} 个，共 ${count} 个`,
  noMatches: "没有匹配",

  settingsLead:
    "Barracuda 编写应用所用的模型、它的修改如何进入你的代码，以及打开的应用的清单。",
  anthropicHint: "你的 Anthropic API 密钥",
  openaiTitle: "OpenAI",
  openaiHint: "任何提供相同 API 的服务",
  urlError: "输入服务的基础 URL，例如 https://api.example.com/v1",
  chars: (n) => `使用 1–${n} 个字符`,
  settingsSaved: "设置已保存",
  discardEdits: "放弃编辑",
  saveSettings: "保存设置",
  modelService: "模型服务",
  modelServiceText: "编辑器的智能体在哪里运行",
  baseUrl: "API 基础 URL",
  apiKey: "API 密钥",
  pasteKey: "粘贴你的密钥",
  hide: "隐藏",
  show: "显示",
  keptHere: "保存在这个浏览器里",
  sentOnlyTo: "只发送给模型服务",
  model: "模型",
  modelId: "模型 ID",
  changes: "修改",
  changesText: "Barracuda 的编辑如何进入你的代码",
  review: "应用前审阅",
  reviewText: "提议的修改在“修改”视图里等你应用",
  tap: "允许 Barracuda 点按预览",
  tapText: "它会在预览设备上检查自己的修改",
  manifest: "打开的应用的清单",
  appName: "应用名称",
  appId: "应用 ID",
  fixed: "固定",
  fixedText: "设备靠 ID 识别应用",
  version: "版本",
  shape: "屏幕形状",
  round: "圆形",
  rectangle: "矩形",
  roundText: "只用内切圆，所以适合每一种屏幕",
  rectText: "用到四角，所以需要矩形屏幕",

  blocked: {
    title: "门户窗口没有打开",
    text: "允许这个网站弹出窗口，然后再次打开门户。",
  },
  silent: {
    title: "门户没有响应",
    text: "检查地址，并确认这台电脑和设备在同一网络中。",
  },
  refused: {
    title: (name) => `${name} 没有安装`,
    text: "没有在门户窗口中安装它。再次打开门户来安装。",
  },
  deviceFailed: "设备无法写入应用",
  doesNotBuild: (message) => `无法构建：${message}`,
  addressError: "输入设备的地址，例如 192.168.1.42:8787",
  downloaded: (folder) => `已下载 ${folder}.zip`,
  downloadedText: (folder) =>
    `解压后，在设备门户的“文件”中新建文件夹 media/apps/${folder}，再把其中的文件上传进去。`,
  portalOpen: "门户已打开",
  portalClosed: "门户已关闭",
  installLead: (name) =>
    `${name} 通过设备门户发送到设备，门户在这个窗口旁边打开。你在那里安装它。`,
  steps: (name) => [
    "打开设备门户",
    `在门户窗口中安装 ${name}`,
    "设备把它写入应用目录",
  ],
  deviceText: "这台电脑所在网络中的一台 Barracuda",
  deviceAddress: "设备地址",
  addressHint: "设备门户打开的地址，例如 ",
  waitingForPortal: "正在等待门户窗口",
  portal: "门户",
  app: "应用",
  builtHere: "在这个浏览器里构建",
  buildingApp: (name) => `正在构建 ${name}`,
  cannotInstall: (name) => `无法安装 ${name}`,
  folder: "文件夹",
  installText: "在门户窗口中选择安装",
  chooseInstall: "门户窗口会显示它收到了什么。在那里选择“安装到设备”。",
  writing: "正在写入 ",
  onDevice: (name) => `${name} 已在设备上`,
  openFromLauncher: "在设备的启动器中打开它。",
  download: (folder) => `下载 ${folder}.zip`,
  backTo: (name) => `返回 ${name}`,
  openPortal: "打开设备门户",
  openPortalAgain: "再次打开门户",

  restored: (n, title) => `已恢复 v${n} · ${title}`,
  changesApplied: "修改已应用",
  storageReset: "存储已清空",
  noAppOpen: "没有打开的应用",
  badFileName: "使用 1–64 个字母、数字、点、短横线或下划线",
  alreadyFile: (name) => `已有文件 ${name}`,

  needsAnthropicKey: "在设置中添加你的 Anthropic API 密钥后开始。",
  needsOpenai: "在设置中添加模型服务的基础 URL 和模型后开始。",
  declined: (detail) => `模型拒绝了这个请求${detail ? `：${detail}` : "。"}`,
  truncated: "回复达到长度上限被截断。请让它分更小的步骤完成。",
  anthropic: {
    auth: "Anthropic 不接受这个 API 密钥。请在设置中检查。",
    permission: "这个 API 密钥不能使用该模型。请在设置中检查模型。",
    notFound: (model) => `Anthropic 没有模型 ${model}。请在设置中检查。`,
    rateLimit: "Anthropic 正在限制这个密钥的请求频率。稍等片刻再发送。",
    badRequest: (message) => `Anthropic 拒绝了请求：${message}`,
    connection: "无法连接 Anthropic。请检查网络连接。",
    failed: (status, message) => `Anthropic 出错（${status}）：${message}`,
  },
  openai: {
    auth: "模型服务不接受这个 API 密钥。请在设置中检查。",
    notFound: (url, model) =>
      `模型服务没有 ${url}，或没有模型 ${model}。请检查设置。`,
    rateLimit: "模型服务正在限制这个密钥的请求频率。稍等片刻再发送。",
    failed: (status, message) => `模型服务出错（${status}）：${message}`,
    connection:
      "无法连接模型服务。请检查基础 URL，并确认它允许来自这个页面的请求。",
  },

  stored: storedZh,
};

export const STRINGS: Record<Lang, Strings> = { en, zh };

const KEY = "le-lang";

/** The words of the language in use; reassigned by `setLang`, so read it when rendering. */
export let t: Strings = en;
export let lang: Lang = "en";

/**
 * The language Live Editor starts in: the website's (`?lang=`, which its links pass), else the one
 * the person picked here, else the browser's first preferred zh or en; English otherwise.
 */
export function initialLang(
  search: string,
  saved: string | null,
  languages: readonly string[],
): Lang {
  const asked = new URLSearchParams(search).get("lang");
  if (asked === "en" || asked === "zh") return asked;
  if (saved === "en" || saved === "zh") return saved;
  const tag = languages.find((tag) => /^(zh|en)\b/i.test(tag)) ?? "en";
  return /^zh/i.test(tag) ? "zh" : "en";
}

/** Uses `next` for every page from now on and remembers it; the shell renders again. */
export function setLang(next: Lang, keep = true) {
  lang = next;
  t = STRINGS[next];
  if (typeof document !== "undefined") document.documentElement.lang = t.tag;
  if (!keep) return;
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // a private window keeps nothing; the choice holds for this visit
  }
}

/** Picks the starting language, and drops `?lang=` from the address once it is followed. */
export function startLang() {
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(KEY);
  } catch {
    // no storage: the browser's languages decide
  }
  const asked = new URLSearchParams(location.search).has("lang");
  setLang(initialLang(location.search, saved, navigator.languages), asked);
  if (asked) {
    const url = new URL(location.href);
    url.searchParams.delete("lang");
    history.replaceState(null, "", url);
  }
}
