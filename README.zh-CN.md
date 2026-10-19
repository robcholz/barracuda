<a href="https://barracuda-os.vercel.app/zh/">
    <img width="1024" alt="Barracuda：以 AI 的速度打造嵌入式产品" src=".github/banner.jpg" />
</a>
&nbsp;

<p align="center">
  <a href="https://barracuda-os.vercel.app/zh/">网站</a>
  ·
  <a href="https://barracuda-os.vercel.app/zh/#product">产品</a>
  ·
  <a href="https://barracuda-os.vercel.app/zh/#ladder">上手</a>
  ·
  <a href="https://barracuda-os.vercel.app/zh/#plugins">插件</a>
  ·
  <a href="https://barracuda-os.vercel.app/zh/boards/">开发板</a>
  ·
  <a href="https://barracuda-os.mintlify.site/zh">文档</a>
  ·
  <a href="https://github.com/robcholz/barracuda/releases">更新日志</a>
  ·
  <a href="README.md">English</a>
</p>

> [!NOTE]
> 在浏览器里烧录，在真硬件上即时验证。[开始安装](https://barracuda-os.mintlify.site/zh/get-started/installation)。

> [!WARNING]
> Barracuda 还在早期的快速开发阶段。插件接口、开发板文件、设置和设备 API 都可能在任意一次提交中改变，目前还没有稳定版本。如果你基于 Barracuda 开发，请固定在某个提交上。

<h1></h1>

## 关于

[Barracuda](https://barracuda-os.vercel.app/zh/) 让你以 AI 的速度打造嵌入式产品。它是一个嵌入式平台，每个功能都是插件。几分钟组合出固件，加上 AI Agent，在真实硬件上即时验证想法。同一套代码跑在 ESP32、STM32、Linux 和 macOS 上。

**8** 个平台 · **80** 块开发板 · **44** 个内置插件 · **6** 种聊天方式

## 安装

[在浏览器中安装](https://barracuda-os.mintlify.site/zh/get-started/installation)：选择开发板，插上 USB，几分钟装好。支持桌面版 Chrome 或 Edge。

也可以用 Rust 工具链从源码构建：

```bash
git clone https://github.com/robcholz/barracuda && cd barracuda
cargo board select && cargo run
```

还没有开发板？选择 `local-linux` 或 `local-macos`，先在电脑上试。每一步都可以在[文档](https://barracuda-os.mintlify.site/zh)里找到。

## 为什么这么快

嵌入式开发通常意味着：每改一次都要编译烧录，固件靠手工拼装，代码绑死在一家厂商上。Barracuda 把这三件事都换掉了。

- **在真硬件上，即时验证。** 描述你要的行为，AI Agent 写好脚本，直接在板子上运行。不用编译，不用烧录：改一句，马上看到结果。[了解实时脚本 →](https://barracuda-os.mintlify.site/zh/use/scripts)
- **每个功能都是插件。** 按产品需要选插件再构建。没选的根本不在固件里；缺的插件，交给你的编程工具来写。[了解插件 →](https://barracuda-os.mintlify.site/zh/plugins/overview)
- **不绑定任何厂商。** 你的插件和脚本不用改，就能跑在 ESP32、STM32、Linux 和 macOS 上。换芯片不用重写；板子还没到，也能先在电脑上开发。[查看支持的开发板 →](https://barracuda-os.vercel.app/zh/boards/)

## 几分钟上手，想走多深都行

六个层级，从和开发板对话，到构建自己的固件。每一级都能独立使用，停在哪里都行。

1. **和开发板对话。** 在浏览器里装好，告诉开发板要做什么。AI Agent 写好脚本，马上运行。
2. **选择功能。** 选上项目需要的功能，比如 Wi-Fi、屏幕、微信，然后构建安装。没选的功能不占内存。
3. **做应用。** 描述一个应用，Live Editor 生成界面和逻辑。预览、修改，满意了用 USB 安装。
4. **接入新硬件。** 让 Claude Code、Cursor 或 Codex 为新的传感器或服务写插件。它遵循和内置功能一样的规则与测试。
5. **接入新开发板。** 告诉编程工具开发板的芯片和外设，它就把开发板接进来，用户随后就能在浏览器里安装。
6. **定制固件。** 告诉编程工具芯片、内存预算和要做的事。它只保留需要的功能，补上缺的，构建出刚好装得下的固件。

## 在常用的应用里和开发板对话

在微信、Telegram、QQ 或网页里都能和开发板对话。每个项目一个独立会话，在哪个应用里都能切换。设置和密钥都存在你自己的开发板上；对话会发送到你选择的模型服务商。

## 把你的开发板接入 Barracuda

接入一次，任何人都能在浏览器里给它装上 Barracuda，并用对话控制它的硬件。一块开发板就是一个短文件，写着芯片、构建目标和内存布局；也可以让你的编程工具用 `create-board` skill 帮你写。阅读[开发板接入指南](https://barracuda-os.mintlify.site/zh/boards/overview)。

## 许可证

Barracuda 采用 [AGPL v3](LICENSE) 许可证。带有自己 `LICENSE` 文件的目录（例如引入的第三方代码）按该文件授权。

如果要在闭源产品中使用 Barracuda，请发邮件到 [finn.robcholz.dev@gmail.com](mailto:finn.robcholz.dev@gmail.com) 咨询商业许可。

## 开源与贡献

Barracuda 是开源的，代码都在这个仓库里：在一个 Rust 仓库里写驱动、应用和定制固件，用你已经在用的编程工具。欢迎社区贡献，从新插件、新开发板到修复和文档都可以。完整的贡献流程请阅读 [CONTRIBUTING.md](CONTRIBUTING.md)。

> [!TIP]
> **和你的编程工具一起开发。** 仓库自带 [skills](.agents/skills/)，也就是现成的说明，教 Claude Code、Cursor 和 Codex 用 `create-plugin` 接入新硬件、用 `create-board` 接入新开发板、用 `create-platform` 接入新芯片。

### 从 Issue 到 PR

提交之前，先[搜索已有的 issue](https://github.com/robcholz/barracuda/issues)，看看你的问题或需求是否已经有人提过。如果没有，请[新建 issue](https://github.com/robcholz/barracuda/issues/new)，写上你用的开发板和复现步骤。你的第一个 PR 合并之前，需要按 [CONTRIBUTING.md](CONTRIBUTING.md#contributor-license-agreement) 的说明接受[贡献者许可协议](CLA.md)。

### 在本地构建

在电脑上从源码构建并运行 Barracuda：

```bash
cargo board select local-linux                      # 或 local-macos，或 boards/configs/ 下的任意开发板
sudo sh platforms/linux/provision.sh barracuda0     # 仅 Linux，每次开机执行一次
BARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run  # 构建并运行
cargo fmt --all --check                             # 格式检查，与 CI 一致
```

工程规范见 [AGENTS.md](AGENTS.md)，包括插件契约、平台架构、代码风格和文档写法。

## 支持与问题

1. 阅读[文档](https://barracuda-os.mintlify.site/zh)，每个层级都有指南，提供简体中文和 [English](https://barracuda-os.mintlify.site) 两种语言。
2. [提交 issue](https://github.com/robcholz/barracuda/issues) 来提问，或者获取开发板相关的帮助。
3. 关注[更新日志](https://github.com/robcholz/barracuda/releases)，了解最新变化。

## 行为准则

我们希望每个人都能互相尊重、体谅他人。Barracuda 遵循[行为准则](CODE_OF_CONDUCT.md)。如需举报违规行为，请发邮件到 [finn.robcholz.dev@gmail.com](mailto:finn.robcholz.dev@gmail.com)。

## 开源依赖

Barracuda 建立在许多开源项目之上，在此特别感谢其中几个：

- [Embassy](https://github.com/embassy-rs/embassy)
- [esp-hal](https://github.com/esp-rs/esp-hal)
- [Lua](https://www.lua.org)
- [picoserve](https://github.com/sammhicks/picoserve)
- [reqwless](https://github.com/drogue-iot/reqwless)
- [embedded-graphics](https://github.com/embedded-graphics/embedded-graphics)
- [heapless](https://github.com/rust-embedded/heapless)
- [Tokio](https://github.com/tokio-rs/tokio)
