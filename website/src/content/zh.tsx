import type { Copy } from "./en";

export const zh: Copy = {
  lang: "zh",
  htmlLang: "zh-Hans",
  meta: {
    title: "Barracuda：以 AI 的速度打造嵌入式产品",
    description:
      "每个功能都是插件的嵌入式平台。几分钟组合出固件，加上 AI Agent，在真实硬件上即时验证想法。",
    boardsTitle: "开发板 · Barracuda",
    boardsDescription:
      "来自 M5Stack、乐鑫、ST 等厂商的 80 块开发板。选一块，在浏览器里烧录，马上在上面验证想法。",
  },
  banner: {
    label: "公告",
    text: "在浏览器里烧录，在真硬件上即时验证。",
    link: "开始安装",
    dismiss: "关闭",
  },
  nav: {
    label: "网站",
    product: "产品",
    how: "上手",
    plugins: "插件",
    boards: "开发板",
    docs: "文档",
    language: "语言",
    languageName: "简体中文",
    getStarted: "开始安装",
  },
  hero: {
    title: (
      <>
        以 <span className="w-accent">AI</span> 的速度
        <br />
        打造嵌入式产品
      </>
    ),
    lead: "Barracuda 是一个嵌入式平台，每个功能都是插件。几分钟组合出固件，加上 AI Agent，在真实硬件上即时验证想法。同一套代码跑在 ESP32、STM32、Linux 和 macOS 上。",
    browserNote: "选择开发板，插上 USB，几分钟装好",
    installButton: "在浏览器中安装",
    startLabel: "开始方式",
    tabBrowser: "浏览器安装",
    tabSource: "从源码构建",
    noteBrowser: "支持桌面版 Chrome 或 Edge",
    noteCopied: "已复制命令",
    noteSource: "需要 Rust 工具链",
    copyLabel: "复制命令",
    readDocs: "阅读文档 →",
    brandsLabel: "与 Barracuda 配合使用的硬件与模型厂商",
  },
  product: {
    title: (
      <>
        为什么这么快。
        <br />
        <span className="w-dim">即时，插件化，跨芯片。</span>
      </>
    ),
    lead: "嵌入式开发通常意味着：每改一次都要编译烧录，固件靠手工拼装，代码绑死在一家厂商上。Barracuda 把这三件事都换掉了。",
    chatTitle: "Web 聊天 · barracuda-a1b2.local",
    chatAsk: "按一下按钮，屏幕就显示当前温度",
    running: "运行中",
    chatReply: "脚本在后台运行了。按下按钮试试。",
    liveTitle: "在真硬件上，即时验证。",
    liveBody:
      "描述你要的行为，AI Agent 写好脚本，直接在板子上运行。不用编译，不用烧录：改一句，马上看到结果。",
    liveLink: "了解实时脚本 →",
    firmwareTitle: "固件 · M5Stack CoreS3",
    plugs: [
      ["Wi-Fi 与设备门户", "在网页里管理设置", true],
      ["AI Agent", "编写并运行脚本", true],
      ["Lua 脚本", "在硬件上实时运行", true],
      ["显示与触摸", "屏幕界面与应用", true],
      ["灯带", "驱动", true],
      ["摄像头", "驱动", false],
      ["Telegram", "聊天应用", false],
    ],
    plugFoot: "关闭的插件不进固件：没有代码，不占内存。",
    plugTitle: "每个功能都是插件。",
    plugBody:
      "按产品需要选插件再构建。没选的根本不在固件里；缺的插件，交给你的编程工具来写。",
    plugLink: "了解插件 →",
    termTitle: "终端 · 一个项目，三种芯片",
    termEspressif: "ESP32-S3 · 乐鑫",
    termSt: "STM32U5 · ST",
    termLaptop: "macOS · 你的电脑",
    termBuilt: "✓ 构建完成",
    termSame: "同样的插件 · 同样的脚本 · 一行不改",
    vendorTitle: "不绑定任何厂商。",
    vendorBody:
      "你的插件和脚本不用改，就能跑在 ESP32、STM32、Linux 和 macOS 上。换芯片不用重写；板子还没到，也能先在电脑上开发。",
    vendorLink: "查看支持的开发板 →",
  },
  audiences: {
    title: (
      <>
        为嵌入式团队、
        <br />
        <span className="w-dim">创客和课堂而做。</span>
      </>
    ),
    items: [
      {
        icon: "code",
        name: "开发者",
        sub: "驱动 · 应用 · 固件",
        body: "在一个 Rust 仓库里写驱动、应用和定制固件，用你已经在用的编程工具。",
        link: "开发者文档",
        to: "develop",
      },
      {
        icon: "lightbulb",
        name: "创客",
        sub: "不写代码也能用",
        body: "聊天就能控制灯、屏幕和传感器，只放进项目用得到的功能。",
        link: "开始安装",
        to: "install",
      },
      {
        icon: "graduationCap",
        name: "教育",
        sub: "浏览器里就能用",
        body: "第一节课就让硬件回应学生，再一步步学会读懂、写出背后的代码。",
        link: "课堂方案",
        to: "#education",
      },
      {
        icon: "cpu",
        name: "开发板厂商",
        sub: "出现在安装页",
        body: "接入一次，任何人都能在浏览器里给你的开发板装上 Barracuda。",
        link: "接入你的开发板",
        to: "#makers",
      },
    ],
  },
  ladder: {
    title: (
      <>
        几分钟上手，
        <br />
        <span className="w-dim">想走多深都行。</span>
      </>
    ),
    lead: "六个层级，从和开发板对话，到构建自己的固件。每一级都能独立使用，停在哪里都行。",
    steps: [
      {
        title: "和开发板对话",
        hint: "所有人 · 浏览器",
        bar: "微信 · Barracuda",
        desc: "在浏览器里装好，告诉开发板要做什么。AI Agent 写好脚本，马上运行。",
        out: "你：让灯每秒闪两次\n\nBarracuda：写好脚本了，正在运行。\n          灯带正在闪，随时说“停”。\n\n你：每天早上 7 点把温度发给我\n\nBarracuda：好了，每天 07:00 发给你。",
      },
      {
        title: "选择功能",
        hint: "创客 · 你的电脑",
        bar: "终端 · 选择功能",
        desc: "选上项目需要的功能，比如 Wi-Fi、屏幕、微信，然后构建安装。没选的功能不占内存。",
        out: "选择要放进固件的功能\n\n [x] Wi-Fi       [x] AI Agent\n [x] 脚本        [x] 灯带\n [x] 屏幕        [ ] 摄像头\n [ ] Telegram    [x] 微信\n\n正在构建并安装到 M5Stack CoreS3…\n完成  ✓  没选的功能不占内存",
      },
      {
        title: "做应用",
        hint: "应用开发者 · 浏览器",
        bar: "Live Editor",
        desc: "描述一个应用，Live Editor 生成界面和逻辑。预览、修改，满意了用 USB 安装。",
        out: "你：做一个番茄钟，屏幕倒计时，结束时灯带闪一下\n\n● 生成 3 个界面、1 个按键事件、1 个灯带动作\n● 实时预览：M5Stack CoreS3\n\n[ 用 USB 安装 ]\n已安装  ✓",
      },
      {
        title: "接入新硬件",
        hint: "开发者 · 你的编程工具",
        bar: "Claude Code",
        desc: "让 Claude Code、Cursor 或 Codex 为新的传感器或服务写插件。它遵循和内置功能一样的规则与测试。",
        out: "> 为 SHT40 温度传感器添加支持\n\n● 读取 skill：create-plugin\n● 创建驱动、文档和设置页\n● 让 AI Agent 能读取这个传感器\n● 运行测试\n\n✓ 全部通过",
      },
      {
        title: "接入新开发板",
        hint: "开发板厂商 · 你的编程工具",
        bar: "Cursor",
        desc: "告诉编程工具开发板的芯片和外设，它就把开发板接进来，用户随后就能在浏览器里安装。其他部分都不用改。",
        out: "> 接入我的 ESP32-S3 开发板\n\n● 读取 skill：create-board\n● 写好芯片、内存布局和外设\n● 构建并在板子上运行\n\n✓ 已出现在安装页",
      },
      {
        title: "定制固件",
        hint: "产品团队 · 你的编程工具",
        bar: "Codex",
        desc: "告诉编程工具芯片、内存预算和要做的事。它只保留需要的功能，补上缺的，构建出刚好装得下的固件。",
        out: "> 为 ESP32-C3（320 KB RAM）构建固件：\n  只要温控和微信\n\n● 选择芯片支持和开发板\n● 保留 9 个插件，去掉 31 个\n● 新写插件：温控\n● 检查 RAM 和存储\n\n✓ 在预算之内",
      },
    ],
  },
  channels: {
    title: (
      <>
        在常用的应用里，
        <br />
        <span className="w-accent">和开发板对话。</span>
      </>
    ),
    lead: "在微信、Telegram、QQ 或网页里都能和开发板对话。每个项目一个独立会话，在哪个应用里都能切换。",
    whatsNew: "看看更新了什么 →",
    installed: "已经安装？重新打开安装页即可更新",
    stats: [
      ["平台", "08"],
      ["开发板", "80"],
      ["内置插件", "44"],
      ["聊天方式", "06"],
    ],
    bullets: [
      ["会话", "：在任何应用里新建、切换、重命名"],
      ["网页聊天", "：随时停下回复并撤回"],
      ["QQ", "：扫码连接，和微信一样"],
      ["网页搜索", "：换成你喜欢的搜索服务"],
    ],
  },
  plugins: {
    title: "只放你需要的",
    lead: "从 Wi-Fi、AI Agent 到聊天应用和屏幕，每个功能都是一个可以开关的插件。没选的插件，不占开发板的内存，也不占存储。",
    families: [
      {
        icon: "wifi",
        title: "Wi-Fi 与设置",
        desc: "联网，然后在网页里管理开发板",
      },
      { icon: "bot", title: "AI Agent", desc: "对话、记忆、规划和工具" },
      { icon: "code", title: "脚本", desc: "运行 AI Agent 写的脚本" },
      {
        icon: "cpu",
        title: "硬件",
        desc: "按键、屏幕、灯带、传感器、摄像头、音频",
      },
      {
        icon: "chat",
        title: "聊天应用",
        desc: "微信、Telegram、QQ、iMessage、网页、终端",
      },
      { icon: "clock", title: "自动化", desc: "定时执行，或在事件发生时执行" },
      { icon: "apps", title: "应用", desc: "安装、打开和更新应用" },
    ],
    allTitle: "浏览全部 44 个插件",
    allBody: "看看每个内置插件做什么、依赖什么。",
  },
  makers: {
    title: (
      <>
        把你的开发板
        <br />
        <span className="w-dim">接入 Barracuda。</span>
      </>
    ),
    lead: "接入一次，任何人都能在浏览器里给它装上 Barracuda，并用对话控制它的硬件。",
    guide: "开发板接入指南 →",
    items: [
      {
        label: "一个短文件",
        note: "一块开发板就是一个文件：芯片、构建目标和内存布局",
        file: "boards/configs/m5stack-cores3/board.yml",
        body: "name: m5stack-cores3\nhardware:\n  chip: esp32s3\ntoolchain:\n  target: xtensa-esp32s3-none-elf\nnative-layout:\n  artifact: partitions.csv",
      },
      {
        label: "或者让 AI 来写",
        note: "在 Claude Code、Cursor 和 Codex 里都能用",
        body: "告诉编程工具开发板的芯片和外设。它写好这个文件和内存布局，然后构建 Barracuda，并在板子上运行。",
      },
      {
        label: "出现在安装页",
        note: "用户直接在浏览器里选到你的开发板",
        body: "用户打开安装页，选中你的开发板，插上就装好了。你不用写安装教程，用户也不用搭环境。",
      },
      {
        label: "外设都交给 AI",
        note: "AI Agent 能直接使用你开发板上的硬件",
        body: "屏幕、灯带、按键、传感器、摄像头：开发板接入后，AI Agent 都能用。用户说一句话，你的开发板就动起来。",
      },
    ],
    supported: "已经支持的开发板",
    seeAll: "查看全部 80 款 →",
    lib: [
      {
        group: "M5Stack",
        items: [
          ["CoreS3", "屏幕、摄像头、麦克风"],
          ["Cardputer", "带键盘的口袋电脑"],
          ["AtomS3R", "拇指大小"],
          ["Tab5", "5 英寸屏幕"],
        ],
      },
      {
        group: "乐鑫 Espressif",
        items: [
          ["ESP32-S3-BOX-3", "语音套件"],
          ["ESP32-S3-DevKitC-1", "通用开发板"],
          ["ESP32-C6-DevKitC-1", "Wi-Fi 6"],
          ["ESP32-P4 Function EV Board", "高性能，适合音视频"],
        ],
      },
      {
        group: "ST 及其他",
        items: [
          ["NUCLEO-U5A5ZJ-Q", "STM32U5"],
          ["TinyPICO", "Unexpected Maker"],
          ["还有 70 多款", "查看完整列表"],
        ],
      },
      {
        group: "电脑",
        items: [
          ["Linux", "没有开发板也能先试"],
          ["macOS", "在 Mac 上同样能跑"],
        ],
      },
    ],
  },
  education: {
    title: (
      <>
        硬件课，
        <br />
        <span className="w-accent">第一节就动手。</span>
      </>
    ),
    lead: "学生在浏览器里安装，第一节课就让开发板回应自己。然后读懂 AI 写的代码，再学着自己写。六个层级，每一级都是一个教学单元。",
    layers: [
      ["不用安装", "学校电脑只要有浏览器"],
      ["先看到结果", "第一节课，开发板就回应学生"],
      ["再看代码", "AI 写的每段脚本都能读、能改"],
      ["继续深入", "应用、驱动、开发板，每一级都是真实的嵌入式开发"],
    ],
    button: "查看六个层级",
    alt: "一块运行 Barracuda 的开发板",
  },
  faq: {
    title: (
      <>
        还有疑问？
        <br />
        <span className="w-dim">先看这几条。</span>
      </>
    ),
    more: "更多问题 →",
    items: [
      {
        q: "我需要会写代码吗？",
        a: "不需要。在浏览器里安装，告诉开发板要做什么，代码由 AI Agent 来写。想自己写的时候，它写的每段代码都能直接读、直接改。",
      },
      {
        q: "AI Agent 和 Live Editor 有什么区别？",
        a: "AI Agent 跑在开发板上，现场写小脚本、马上运行，适合快速验证。Live Editor 跑在浏览器里，生成带界面的完整应用，再安装到开发板上。",
      },
      {
        q: "支持哪些开发板？",
        a: "80 款开发板，来自 M5Stack、乐鑫、ST 等厂商，覆盖 ESP32 和 STM32。手头没有开发板？先在 Linux 或 Mac 上试。",
      },
      {
        q: "我的数据和密钥存在哪里？",
        a: "设置和密钥都存在你自己的开发板上。对话会发送到你选择的模型服务商，或 Barracuda 自己的模型服务。",
      },
      {
        q: "能用 Claude Code、Cursor 或 Codex 吗？",
        a: "能。仓库里自带 skills，也就是现成的说明，教它们接入新硬件、适配开发板和芯片、构建定制固件。",
      },
    ],
  },
  updates: {
    title: "最近更新",
    all: "查看全部更新 →",
    items: [
      ["2026-10-10", "每个聊天应用都有独立会话", 85],
      ["2026-10-09", "网页聊天：停止并撤回回复", 81],
      ["2026-10-09", "全新网页聊天，QQ 扫码连接", 80],
      ["2026-09-20", "Wi-Fi 配网与设备门户设置页", 77],
    ],
  },
  closing: {
    title: (
      <>
        开发更快。
        <br />
        即时验证。
        <br />
        <span className="w-accent">处处能跑。</span>
      </>
    ),
    titleLineHeight: 86,
    lead: "在浏览器里烧录开发板，几分钟就能验证想法。想构建自己的固件，也只要一条命令。",
    install: "在浏览器中安装",
    docs: "阅读文档",
  },
  footer: {
    tagline: "每个功能都是插件、内置 AI 的嵌入式平台。",
    product: "产品",
    install: "在浏览器中安装",
    boards: "开发板",
    plugins: "插件",
    changelog: "更新日志",
    developers: "开发者",
    docs: "文档",
    writePlugin: "编写插件",
    partners: "合作",
    boardMakers: "开发板厂商",
    education: "教育",
    community: "社区",
    issue: "问题反馈",
  },
  boards: {
    count: "块开发板",
    lead: "来自 M5Stack、乐鑫、ST 等厂商，同一套代码也能在 Linux 和 macOS 上运行。选一块，在浏览器里烧录，马上在上面验证想法。",
    searchLabel: "搜索开发板",
    placeholder: "CoreS3、BOX-3、TinyPICO…",
    filterLabel: "按芯片筛选",
    all: "全部",
    allChips: "全部芯片",
    shown: "块",
    head: ["开发板", "厂商", "芯片", "型号"],
    go: "安装说明",
    emptyTitle: "没有匹配的开发板",
    emptyBody: "换一个名称，或显示全部芯片。",
    clear: "清除筛选",
    ctaTitle: "你是开发板厂商？",
    ctaBody:
      "用一个短文件接入你的开发板，或者让编程工具用 create-board 来写。接入后它会出现在这里，任何人都能在浏览器里给它装上 Barracuda。",
    ctaButton: "接入你的开发板",
  },
};
