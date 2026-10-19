import type { IconName } from "@/components/Icon";
import type { Lang } from "@/lib/links";

export const en = {
  lang: "en" as Lang,
  htmlLang: "en",
  meta: {
    title: "Barracuda: build embedded products at the speed of AI",
    description:
      "An embedded platform where every feature is a plugin. Compose firmware in minutes, add an AI agent, and test ideas live on real hardware.",
    boardsTitle: "Boards · Barracuda",
    boardsDescription:
      "80 boards from M5Stack, Espressif, ST, and more. Pick one, flash it from the browser, and test ideas on it live.",
  },
  banner: {
    label: "Announcement",
    text: "Flash a board from your browser and test ideas on it live.",
    link: "Get started",
    dismiss: "Dismiss",
  },
  nav: {
    label: "Site",
    product: "Product",
    how: "How it works",
    plugins: "Plugins",
    boards: "Boards",
    docs: "Docs",
    language: "Language",
    languageName: "English",
    getStarted: "Get started",
  },
  hero: {
    title: (
      <>
        Build embedded products
        <br />
        at the speed{" "}
        <span style={{ whiteSpace: "nowrap" }}>
          of <span className="w-accent">AI.</span>
        </span>
      </>
    ),
    lead: "Barracuda is an embedded platform where every feature is a plugin. Compose firmware in minutes, add an AI agent, and test ideas live on real hardware. The same code runs on ESP32, STM32, Linux, and macOS.",
    browserNote: "Pick your board, plug it in, done in minutes",
    installButton: "Install in browser",
    startLabel: "How to start",
    tabBrowser: "In the browser",
    tabSource: "From source",
    noteBrowser: "Works in desktop Chrome or Edge",
    noteCopied: "Command copied",
    noteSource: "Needs the Rust toolchain",
    copyLabel: "Copy command",
    readDocs: "Read the docs →",
    brandsLabel: "Hardware and model companies Barracuda works with",
  },
  product: {
    title: (
      <>
        What makes it fast.
        <br />
        <span className="w-dim">Live. Modular. Any chip.</span>
      </>
    ),
    lead: "Embedded work usually means an edit-compile-flash loop for every change, firmware built by hand, and code tied to one vendor. Barracuda replaces all three.",
    chatTitle: "Web chat · barracuda-a1b2.local",
    chatAsk: "Press the button to show the temperature on screen",
    running: "Running",
    chatReply: "The script is running in the background. Try the button.",
    liveTitle: "Test on real hardware, instantly.",
    liveBody:
      "Describe a behavior and the AI agent writes a script and runs it on the board. No compiling, no flashing: change it and see the result right away.",
    liveLink: "How live scripts work →",
    firmwareTitle: "Firmware · M5Stack CoreS3",
    plugs: [
      ["Wi-Fi and Device Portal", "Settings in a web page", true],
      ["AI agent", "Writes and runs scripts", true],
      ["Lua scripts", "Live on the hardware", true],
      ["Display and touch", "Screen UI and apps", true],
      ["LED strip", "Driver", true],
      ["Camera", "Driver", false],
      ["Telegram", "Chat app", false],
    ] as [string, string, boolean][],
    plugFoot: "Off means not in the firmware: no code, no memory.",
    plugTitle: "Every feature is a plugin.",
    plugBody:
      "Pick what this product needs and build. What you leave out isn’t in the firmware at all, and your coding agent writes the plugins you’re missing.",
    plugLink: "Explore plugins →",
    termTitle: "Terminal · one project, three chips",
    termEspressif: "ESP32-S3 · Espressif",
    termSt: "STM32U5 · ST",
    termLaptop: "macOS · your laptop",
    termBuilt: "✓ built",
    termSame: "Same plugins · same scripts · no changes",
    vendorTitle: "No vendor lock-in.",
    vendorBody:
      "Your plugins and scripts run unchanged on ESP32, STM32, Linux, and macOS. Switch chips without a rewrite, or start on your laptop before the board arrives.",
    vendorLink: "See supported boards →",
  },
  audiences: {
    title: (
      <>
        Made for embedded teams,
        <br />
        <span className="w-dim">makers, and classrooms.</span>
      </>
    ),
    items: [
      {
        icon: "code",
        name: "Developers",
        sub: "Drivers · apps · firmware",
        body: "Write drivers, apps, and custom firmware in one Rust repo, with the coding tools you already use.",
        link: "Developer docs",
        to: "develop",
      },
      {
        icon: "lightbulb",
        name: "Makers",
        sub: "No code needed",
        body: "Control lights, screens, and sensors by chatting. Include only the features your project uses.",
        link: "Get started",
        to: "install",
      },
      {
        icon: "graduationCap",
        name: "Education",
        sub: "Runs in the browser",
        body: "Students make hardware respond in the first lesson, then learn to read and write the code behind it.",
        link: "For classrooms",
        to: "#education",
      },
      {
        icon: "cpu",
        name: "Board makers",
        sub: "Listed on the install page",
        body: "Add your board once, and anyone can install Barracuda on it from the browser.",
        link: "Add your board",
        to: "#makers",
      },
    ] as {
      icon: IconName;
      name: string;
      sub: string;
      body: string;
      link: string;
      to: "develop" | "install" | "#education" | "#makers";
    }[],
  },
  ladder: {
    title: (
      <>
        Start in minutes.
        <br />
        <span className="w-dim">Go as deep as you want.</span>
      </>
    ),
    lead: "Six levels, from chatting with your board to building your own firmware. Each one works on its own, so stop wherever you like.",
    steps: [
      {
        title: "Chat with your board",
        hint: "Anyone · in the browser",
        bar: "WeChat · Barracuda",
        desc: "Install from the browser, then tell your board what to do. The AI agent writes a script and runs it right away.",
        out: "You: blink the light twice a second\n\nBarracuda: Wrote a script, it’s running.\n           The strip is blinking now. Say stop anytime.\n\nYou: send me the temperature every morning at 7\n\nBarracuda: Done. I’ll send it every day at 07:00.",
      },
      {
        title: "Choose your features",
        hint: "Makers · on your computer",
        bar: "Terminal · choose features",
        desc: "Pick the features your project needs, such as Wi-Fi, a screen, or WeChat, then build and install. Features you leave out take no memory.",
        out: "Choose the features to include\n\n [x] Wi-Fi       [x] AI agent\n [x] Scripts     [x] LED strip\n [x] Display     [ ] Camera\n [ ] Telegram    [x] WeChat\n\nBuilding and installing on M5Stack CoreS3…\nDone  ✓  Skipped features use no memory",
      },
      {
        title: "Build apps",
        hint: "App makers · in the browser",
        bar: "Live Editor",
        desc: "Describe an app and the Live Editor builds its screens and logic. Preview it, refine it, and install it over USB.",
        out: "You: build a Pomodoro timer, countdown on screen, strip flashes when done\n\n● Built 3 screens, 1 button event, 1 strip action\n● Live preview: M5Stack CoreS3\n\n[ Install over USB ]\nInstalled  ✓",
      },
      {
        title: "Add new hardware",
        hint: "Developers · your coding tool",
        bar: "Claude Code",
        desc: "Ask Claude Code, Cursor, or Codex to add support for a new sensor or service. Its work follows the same rules and tests as the built-in features.",
        out: "> Add support for the SHT40 temperature sensor\n\n● Read skill: create-plugin\n● Create the driver, docs, and settings page\n● Let the AI agent read the sensor\n● Run tests\n\n✓ All passed",
      },
      {
        title: "Add a new board",
        hint: "Board makers · your coding tool",
        bar: "Cursor",
        desc: "Tell your coding tool the board’s chip and parts, and it adds the board. Users can then install on it from the browser. Nothing else has to change.",
        out: "> Add my ESP32-S3 board\n\n● Read skill: create-board\n● Write the chip, memory layout, and parts\n● Build and run on the board\n\n✓ Listed on the install page",
      },
      {
        title: "Build custom firmware",
        hint: "Product teams · your coding tool",
        bar: "Codex",
        desc: "Give your coding tool the chip, the memory budget, and the job. It keeps only the features you need, writes what’s missing, and builds firmware that fits.",
        out: "> Build firmware for an ESP32-C3 with 320 KB RAM:\n  thermostat and WeChat only\n\n● Pick the chip support and board\n● Keep 9 plugins, drop 31\n● Write a new plugin: thermostat\n● Check RAM and storage\n\n✓ Fits the budget",
      },
    ],
  },
  channels: {
    title: (
      <>
        Chat from the apps
        <br />
        <span className="w-accent">you already use.</span>
      </>
    ),
    lead: "Talk to your board from WeChat, Telegram, QQ, or the web. Keep a separate conversation for each project and switch between them in any app.",
    whatsNew: "See what’s new →",
    installed: "Already installed? Reopen the installer to update",
    stats: [
      ["Platforms", "08"],
      ["Boards", "80"],
      ["Built-in plugins", "44"],
      ["Ways to chat", "06"],
    ],
    bullets: [
      ["Conversations", ": start, switch, and rename them in any app"],
      ["Web chat", ": stop a reply midway and undo it"],
      ["QQ", ": connect by scanning a QR code, like WeChat"],
      ["Web search", ": use the search provider you prefer"],
    ],
  },
  plugins: {
    title: "Only what you need",
    lead: "Every feature, from Wi-Fi and the AI agent to chat apps and screens, is a plugin you turn on or off. Leave one out and it takes no memory or storage on your board.",
    families: [
      {
        icon: "wifi",
        title: "Wi-Fi and settings",
        desc: "Get online, then manage the board from a web page",
      },
      {
        icon: "bot",
        title: "AI agent",
        desc: "Conversations, memory, planning, and tools",
      },
      {
        icon: "code",
        title: "Scripts",
        desc: "Run the scripts the AI agent writes",
      },
      {
        icon: "cpu",
        title: "Hardware",
        desc: "Buttons, screens, LED strips, sensors, cameras, audio",
      },
      {
        icon: "chat",
        title: "Chat apps",
        desc: "WeChat, Telegram, QQ, iMessage, web, terminal",
      },
      {
        icon: "clock",
        title: "Automations",
        desc: "Run tasks on a schedule or when something happens",
      },
      { icon: "apps", title: "Apps", desc: "Install, open, and update apps" },
    ] as { icon: IconName; title: string; desc: string }[],
    allTitle: "Browse all 44 plugins",
    allBody: "See what each built-in plugin does and what it needs.",
  },
  makers: {
    title: (
      <>
        Bring your board
        <br />
        <span className="w-dim">to Barracuda.</span>
      </>
    ),
    lead: "Add your board once. Anyone can then install Barracuda on it from the browser and control its hardware by chatting.",
    guide: "Board guide →",
    items: [
      {
        label: "One short file",
        note: "A board is one file: its chip, build target, and memory layout",
        file: "boards/configs/m5stack-cores3/board.yml",
        body: "name: m5stack-cores3\nhardware:\n  chip: esp32s3\ntoolchain:\n  target: xtensa-esp32s3-none-elf\nnative-layout:\n  artifact: partitions.csv",
      },
      {
        label: "Or let AI write it",
        note: "Works in Claude Code, Cursor, and Codex",
        body: "Tell your coding tool the board’s chip and parts. It writes the file and the memory layout, then builds Barracuda and runs it on the board.",
      },
      {
        label: "Listed on the install page",
        note: "Users pick your board right in the browser",
        body: "Users open the install page, pick your board, plug it in, and they’re done. No install guide for you to write, no tools for them to set up.",
      },
      {
        label: "Every part usable by AI",
        note: "The AI agent can use your board’s hardware",
        body: "Screens, LED strips, buttons, sensors, cameras: once your board is added, the AI agent can use them all. One sentence from a user, and your board comes alive.",
      },
    ] as { label: string; note: string; file?: string; body: string }[],
    supported: "Supported boards",
    seeAll: "See all 80 →",
    lib: [
      {
        group: "M5Stack",
        items: [
          ["CoreS3", "Screen, camera, mic"],
          ["Cardputer", "Pocket computer with a keyboard"],
          ["AtomS3R", "Thumb-sized"],
          ["Tab5", "5-inch screen"],
        ],
      },
      {
        group: "Espressif",
        items: [
          ["ESP32-S3-BOX-3", "Voice kit"],
          ["ESP32-S3-DevKitC-1", "General-purpose board"],
          ["ESP32-C6-DevKitC-1", "Wi-Fi 6"],
          ["ESP32-P4 Function EV Board", "Fast, for video and audio"],
        ],
      },
      {
        group: "ST and more",
        items: [
          ["NUCLEO-U5A5ZJ-Q", "STM32U5"],
          ["TinyPICO", "Unexpected Maker"],
          ["70+ more", "See the full list"],
        ],
      },
      {
        group: "Computers",
        items: [
          ["Linux", "Try it before you have a board"],
          ["macOS", "Runs on your Mac too"],
        ],
      },
    ],
  },
  education: {
    title: (
      <>
        Hardware class,
        <br />
        <span className="w-accent">from lesson one.</span>
      </>
    ),
    lead: "Students install from the browser and make the board respond in the first lesson. Then they read the code the AI wrote and learn to write their own. Each of the six levels is a unit of the course.",
    layers: [
      ["Nothing to install", "School computers need only a browser"],
      ["Results first", "The board responds to students in lesson one"],
      [
        "Then the code",
        "Every script the AI writes is there to read and change",
      ],
      [
        "Go further",
        "Apps, drivers, boards: real embedded work at every level",
      ],
    ],
    button: "See the six levels",
    alt: "A board running Barracuda",
  },
  faq: {
    title: (
      <>
        Questions?
        <br />
        <span className="w-dim">Start here.</span>
      </>
    ),
    more: "More questions →",
    items: [
      {
        q: "Do I need to know how to code?",
        a: "No. Install from the browser and tell your board what to do; the AI agent writes the code. When you want to write your own, everything it wrote is there to read and change.",
      },
      {
        q: "How is the AI agent different from the Live Editor?",
        a: "The AI agent runs on your board. It writes small scripts and runs them right away, so it suits quick tasks. The Live Editor runs in your browser. It builds complete apps with screens, which you then install on your board.",
      },
      {
        q: "Which boards are supported?",
        a: "80 boards from M5Stack, Espressif, ST, and more, with ESP32 and STM32 chips. No board yet? Try it on Linux or a Mac first.",
      },
      {
        q: "Where are my data and keys stored?",
        a: "Your settings and keys stay on your board. Conversations go to the AI provider you choose, or to Barracuda’s own model service.",
      },
      {
        q: "Can I use Claude Code, Cursor, or Codex?",
        a: "Yes. The repository includes skills, ready-made instructions that teach them to add hardware, add boards and chips, and build custom firmware.",
      },
    ],
  },
  updates: {
    title: "Recent updates",
    all: "See all updates →",
    items: [
      ["2026-10-10", "Separate conversations in every chat app", 85],
      ["2026-10-09", "Web chat: stop and undo replies", 81],
      ["2026-10-09", "A new web chat, and QQ setup by QR code", 80],
      ["2026-09-20", "Wi-Fi setup and the Device Portal settings page", 77],
    ] as [string, string, number][],
  },
  closing: {
    title: (
      <>
        Build fast.
        <br />
        Test live.
        <br />
        <span className="w-accent">Run anywhere.</span>
      </>
    ),
    titleLineHeight: 74,
    lead: "Flash a board from the browser and test ideas on it in minutes. When you’re ready to build your own firmware, it’s one command.",
    install: "Install in browser",
    docs: "Read the docs",
  },
  footer: {
    tagline:
      "The embedded platform where every feature is a plugin, with AI built in.",
    product: "Product",
    install: "Install in browser",
    boards: "Boards",
    plugins: "Plugins",
    changelog: "Changelog",
    developers: "Developers",
    docs: "Docs",
    writePlugin: "Write a plugin",
    partners: "Partners",
    boardMakers: "Board makers",
    education: "Education",
    community: "Community",
    issue: "Report an issue",
  },
  boards: {
    count: "boards",
    lead: "From M5Stack, Espressif, ST, and more, and the same code runs on Linux and macOS. Pick a board, flash it from the browser, and test ideas on it live.",
    searchLabel: "Search boards",
    placeholder: "CoreS3, BOX-3, TinyPICO…",
    filterLabel: "Filter by chip",
    all: "All",
    allChips: "All chips",
    shown: "boards",
    head: ["Board", "Vendor", "Chip", "ID"],
    go: "Setup guide",
    emptyTitle: "No matching boards",
    emptyBody: "Try another name, or show all chips.",
    clear: "Clear filters",
    ctaTitle: "Make boards?",
    ctaBody:
      "Add your board with one short file, or let your coding agent write it with create-board. Then it’s listed here, and anyone can install Barracuda on it from the browser.",
    ctaButton: "Add your board",
  },
};

export type Copy = typeof en;
