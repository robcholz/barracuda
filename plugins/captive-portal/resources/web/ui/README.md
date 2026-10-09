# Portal UI kit

Build-time helpers for portal pages. A contributor imports the kit from its
`resources/web/entry.ts`; `tools/web/build.ts` bundles the parts it uses into that
plugin's own `filesystem/resources/entry.js`. The shell never loads the kit at
runtime and contributors never share code at runtime.

```ts
import {
  definePage,
  header,
  settingsForm,
} from "../../../captive-portal/resources/web/ui";
```

The kit draws the design system's markup (`.bc-*` classes from the shell's one
stylesheet); a page adds no stylesheet of its own unless it needs something the
design system lacks, and then removes it on cleanup.

## The module contract

A page exports `mount(root, context)`; the shell gives each mount its own `root`
and a `PortalContext`:

| member            | meaning                                                                                    |
| ----------------- | ------------------------------------------------------------------------------------------ |
| `signal`          | aborted on navigation, language change and unload: pass it to `fetch` and listeners        |
| `lang`            | `"zh"` or `"en"`; render every string in it (a language change remounts the page)          |
| `toast(toast)`    | the shell's stack, bottom-right: `{ kind, title, body?, code?, action? }`                  |
| `navigate(id)`    | go to another entry ID, or `"overview"`                                                    |
| `status(id?)`     | the entry's latest `GET /portal/status` record (this page's own by default), or `null`     |
| `refreshStatus()` | reads the status again and redraws the sidebar and overview; call it after a save succeeds |

`mount` may return a cleanup function (or a promise of one). After the signal
aborts, `toast`, `navigate` and `refreshStatus` do nothing. Types: `PortalModule`,
`PortalContext`, `Toast`, `Lang`, `PortalText`, `EntryStatus`, `EntryState` (all
exported by the kit).

An `EntryStatus` is `{ state, label?, detail? }`: `state` is `"ready"`, `"attention"`
or `"off"`, `label` a `{ zh, en }` phrase (「已配置」), `detail` a machine value the
shell shows in mono (a network name). Every page that saves calls
`void context.refreshStatus()` once the device accepts, so the sidebar, the top bar
badge and the 「开始使用」 steps follow without a reload.

## Exports

Text and DOM (`dom.ts`)

- `Text` is `string | { zh, en }`; `pick(text, lang)` returns the string.
- `h(tag, props, ...children)` builds an element: `class`, `on<event>` listeners,
  boolean attributes; string children are text, never markup.
- `icon(ICON_*, size?)`: a Lucide icon (`ICON_EYE`, `ICON_REFRESH`, `ICON_PLUS`,
  `ICON_WIFI`, `ICON_COPY`, `ICON_EXTERNAL_LINK`, `ICON_MAIL`, `ICON_KEY_ROUND`, … from
  `icons.ts`), 16px by default, 18px on phone rows.
- `mark(MARK_*, size?)`: a design mark from `marks.ts` — `MARK_BARE`, `MARK_TILE`
  (chat avatar), `MARK_OPENAI`, `MARK_ANTHROPIC`, `MARK_TELEGRAM`, `MARK_WECHAT`,
  `MARK_QQ`, `MARK_BLUEBUBBLES`.
- `assetIcon(url, size?)`: an icon file; `.svg` is drawn as a `currentColor` mask,
  anything else as an image. Use `new URL("./icon.svg", import.meta.url).href` for
  the page's own icon.
- `twoDigits(n)`: counts and step numbers (`04`).

Layout (`layout.ts`)

- `definePage(render)`: a `mount` that renders `render(context)` once into the root
  and empties it on cleanup.
- `page(...children)`: the page column (`.bc-page`).
- `header({ title, lead, icon?, extra?, figure?, figureLabel?, figureWidth?, scan? }, lang)`:
  the header frame. `figure` is an `<hl-figure>` name: `riffle` (built in) or the
  page's own entry ID, whose manifest `figure` module the shell loads. `scan`
  starts the router's sweep (also settable later with `setAttribute("scan", "true")`).
- `kv(rows, lang, { live?, mono?, labelWidth? })`: a `.bc-kv` table; `live` makes it
  a polite status region.
- `term(label, tip, lang, { start? })`: the dotted-underline aside (`只写`, `明文`).
- `badge(label, lang, { signal? })`, `button(label, lang, { variant, size, icon, type, onClick })`,
  `frame(...children)`, `row(title, hint, lang, ...children)` (a label-left row).

Settings form (`settings.ts`)

- `settingsForm(options, context)` returns `{ element, values(), setError(name, message, code?), submit(), footer }`.
  - `options.rows`: `{ title, hint?, link?, fields, blocks? }[]`, each a label-left row;
    `link: { label, href }` is an external link under the hint (「打开 @BotFather」, new tab);
    `blocks` are nodes after the fields in the row body (a `resultCard`, a `qrLink`).
  - `options.advanced`: `{ hint?, fields, open?, columns?, onToggle? }`, the 「高级」 fold
    (hint defaults to 「已填入默认值」); it opens by itself when one of its fields is invalid.
    `onToggle(open)` runs whenever it opens or closes (WeChat shows its footer only while open).
  - `options.endpoint`: the POST target, shown in mono in the footer with the
    「只写」 term; the footer also has 「清空」 (restores defaults) and the submit button.
  - `options.submit`: the button label, a verb naming the result.
  - `options.body(values)`: shapes the JSON body (default: the values object;
    wrap it, for example `(v) => [v]`, when the endpoint takes a batch).
  - `options.success`: overrides the success toast (`title`, `body`, `action`).
  - `options.onSuccess(values)`: runs after a 2xx.
  - `options.onError(error)`: runs on a non-2xx with the device's `DeviceError`
    (`{ status, error, message?, code? }`, see below) before the toast; return
    `{ title?, body? }` to replace the toast's, typically `{ body: error.message }`, and
    call `setError` to put an upstream refusal on its field.
  - `setError(name, message, code?)`: marks a field; `code` follows the message in mono
    (「QQ 开放平台：机器人不存在 · `10004`」). `footer` is the footer element, for pages
    that show it only in some states (hide it with `style.display = "none"`).
- Fields, all with `name`, `label`, optional `hint` and `optional`:
  - `{ kind: "text" | "url", value?, placeholder?, mono? }` — trimmed; `url` must be http(s);
  - `{ kind: "secret", placeholder? }` — password input with 显示/隐藏, never prefilled,
    cleared after the device accepts it and when the page goes away;
  - `{ kind: "number", value?, unit?, min? = 0, max? = 4294967295 }` — integer, unit in an addon;
  - `{ kind: "select", options: { value, label }[], value? }`;
  - `{ kind: "switch", value? }` — a switch row, sent as a boolean;
  - `{ kind: "radio", options: { value, label, hint?, icon? }[], value?, columns? = 2 }` —
    radio cards; `icon` is an `ICON_*` or `MARK_*` string for the 40px tile.
  - text, URL and secret fields take `action: Node`, a button on the input's line after
    it (and after 显示/隐藏), such as 「验证」 or 「测试连接」.
- Validation runs on submit: an invalid field gets a red border and a message
  under it (「请填写 Bot Token。」), focus moves to the first one, and nothing is
  sent. Typing clears the field's message. Empty optional fields are left out.
- `submitJson(context, { endpoint, body, method?, timeoutMs?, success?, retry?, onError? })`
  posts JSON (`redirect: "error"`, `cache: "no-store"`, 15 s timeout, aborted with
  the page) and toasts the outcome: 2xx 「设备已接受配置」; 404 「接口不可用」;
  other 4xx 「配置被拒绝」; 5xx 「提交失败」 (status in mono); no reply
  「未收到设备确认 · 配置可能已生效。」 with 「重试」 when `retry` is given.
  Resolves `"accepted" | "rejected" | "failed" | "aborted"`; nothing is toasted
  after the page is gone. `settingsForm` uses it and allows one request at a time.
- `fieldControl(field, lang)`: one field on its own, for pages that lay out
  controls themselves.
- `fitColumns(count, min, gap)` (`grid.ts`): a `grid-template-columns` value with
  `count` equal columns that drops to fewer, then one, when a column would be
  narrower than `min` px; no media query, so it works inline. Radio cards (180px)
  and a multi-column 「高级」 fold (160px) use it, so they stack on a phone.
- `KIT_STRINGS[lang]`: the kit's copy, if a custom page wants the same words.

Device calls (`device.ts`), for flows beyond one POST (QR login, signup codes)

- `callDevice<T>(context, endpoint, { method? = "GET", body?, timeoutMs? = 15 s })`
  sends JSON when `body` is given (`redirect: "error"`, `cache: "no-store"`, aborted with
  the page) and resolves `{ kind: "ok", status, data }` (parsed JSON, `null` for 204),
  `{ kind: "error", error: DeviceError }`, `{ kind: "offline" }` (no reply) or
  `{ kind: "aborted" }` (report nothing). It never toasts.
- `DeviceError` is `{ status, error, message?, code?, retry? }`, read from the device's
  `{"error", "message"?, "code"?, "retry"?}` body by `deviceError(response)`; `message`
  and `code` are the upstream service's own words. A numeric `code` becomes a string.
  `retry: true` means the device kept what it had and the same request resumes it.
- `toastDeviceError(context, result, retry?)` toasts an `error` or `offline` result in the
  kit's words: 「配置被拒绝」 (4xx), 「接口不可用」 (404), 「提交失败」 (5xx) with the
  status (and upstream code) in mono and `message` as the body; 「未收到设备确认」 with
  「重试」 when nothing came back. An error with `retry: true` also gets 「重试」 when
  `retry` is given.

Channel state (`channel.ts`)

- `readChannel<T>(context, endpoint)`: `GET` a channel's config path, which answers
  `{"configured": bool, …}` and never settings or keys. Resolves the reply (`T` adds
  page-specific fields, such as Inkbox's `signup`), or `null` when the device gave none
  in that shape. It never toasts.
- `configuredRow(name, lang)` returns `{ element, show(visible) }`: a 「通道」 row with
  the result card `name` and the 已配置 badge, hidden until `show(true)`. Put it first in
  the form; show it when `readChannel` says `configured`, and after a save succeeds:

  ```ts
  const current = configuredRow("Telegram", lang);
  form.element.prepend(current.element);
  void readChannel(context, "/api/gateway/telegram").then((state) => {
    if (state?.configured) current.show(true);
  });
  // settingsForm options: onSuccess: () => { current.show(true); void context.refreshStatus(); }
  ```

Channel mode and allowed accounts (`inbound.ts`), for the external channel pages

Every channel serves the same JSON under its config path: `GET <endpoint>` answers
`ChannelStatus` (`{configured, mode, receive?: {state, message?, capacity?}, owners: {count}}`),
`POST <endpoint>/mode` takes `{mode}`, and `GET`/`POST <endpoint>/owners` answer `OwnersReply`
(`{owners: [{id, label}], pairing: {code, expires_in} | null, ignored}`) and take
`{"remove": id}` or `{"rotate": true}`.

- `channelInbound(context, { endpoint, channel, how, command?, modes?, pollMs? = 5000 })` returns
  `{ rows, attach(form), apply(status), refresh() }`: the two rows below, wired to the device.
  - `attach(form)` puts them before the form's 「高级」 fold (or its footer).
  - `apply(status)` shows the state the page's own `readChannel` read; `refresh()` reads it again
    (call it after a save succeeds). Both rows show only while `configured`.
  - A mode change reads the channel again, then calls `context.refreshStatus()`. In `send_receive`
    the channel is read every `pollMs` until the page goes away, so the badge stays live; a
    change of receive state there refreshes the shell's status too, and a change of `owners.count`
    reads the list again. A missed read keeps what is shown.

  ```ts
  const inbound = channelInbound(context, {
    endpoint: "/api/gateway/telegram",
    channel: "Telegram",
    how: { zh: "在 Telegram 里给 Bot 发送", en: "In Telegram, send the bot" },
    command: (code) => `/start ${code}`,
  });
  inbound.attach(form);
  void readChannel<ChannelStatus>(context, ENDPOINT).then(inbound.apply);
  // settingsForm options: onSuccess: () => void inbound.refresh()
  ```

- `modeRow({ endpoint, channel, modes?, onChange? }, context)` returns `{ element, update(status) }`:
  the 「模式」 row, one radio card per mode in `modes` (default `CHANNEL_MODES`: 停用 / 仅发送 /
  收发; WeChat passes `["disabled", "send_receive"]`). Under the cards, the state badge: 收发中
  (signal) for `receiving`, 连接中 for `starting` and `idle`, 等待名额 with the slot count
  (`名额 N/N`, mono, a `.bc-term` saying what sets it) and the alert 「收发通道已达上限（N）」 for
  `no_slot`, 连接中断 with the device's `message` in mono for `error`; 仅发送 and 已停用 for the
  other modes. Choosing posts `{mode}` at once; a 204 or a 409 `no_slot` (the mode is saved) runs
  `onChange`, any other answer is toasted and the choice goes back.
- `accountsRow({ endpoint, how, command? }, context)` returns `{ element, load(), hide(), count() }`:
  the 「授权账号」 row. A card holds the binding code (`.bc-page-title bc-mono`, `command(code)` when
  given) under `how`, its countdown (「有效期还剩 m:ss」, counted locally; the list is read again when
  it ends) and 「换一个绑定码」 (`{"rotate": true}`); `pairing: null` drops that box. Then each account,
  `label` over its `id` in mono (the `id` alone, in mono, without a label), with 「移除」
  (`{"remove": id}`, no confirmation), or 「还没有授权账号」. Refusals are toasted; a success reads
  the list again.
- Types: `ChannelMode`, `ReceiveState`, `ChannelStatus`, `Owner`, `OwnersReply`, and the options
  and handles above.

Blocks (`blocks.ts`): the design's form blocks, for a row's `blocks` or anywhere

- `resultCard({ title, badge, sub?, initial?, rows?, action? }, lang)`: what a check found
  (`role="status"`): a 40px tile with `initial` (a check mark without one), the title, `sub`
  in mono, the green badge, key-value `rows` (mono values) and one `action` node.
- `stepList(steps, { current, done }, lang)`: numbered steps; the first `done` show a check
  on the signal fill, `current` (`aria-current="step"`, or `-1` for none) is outlined in ink.
- `note(text, lang, { mono?, action?: { label, onClick } })`: a small muted line with a
  mono value and a link-styled button after a dot (「验证码已发到 `you@example.com` · 重新发送」).
- `qrPlate(data, px, lang, { dim?, padding? = 8 })`: a QR Code on a plate that stays light
  in the dark theme (`data-theme="light"`); `data: null` is the empty plate while a code
  loads; `dim` fades the code under a label (「已扫码」, 「二维码已过期」).
- `qrLink(url, label, lang, { px? = 120 })`: a link to open on a phone: its code, the URL
  in mono and an outline button that opens it in a new tab.
- `copyText(text)`: copies to the clipboard, falling back to a selection copy where
  `navigator.clipboard` is missing (plain HTTP); resolves whether it worked.

QR Codes (`qr.ts`)

- `encodeQr(text, mask?)`: byte mode (UTF-8), level M, versions 1–40, the smallest that
  fits (throws a `RangeError` past 2331 bytes); returns `{ version, mask, size, modules }`
  with `modules[y * size + x]` 1 for dark. Masks are chosen by the standard's penalty rules.
- `qrPath(code, quiet? = 2)`: the dark modules as one SVG path (one rectangle per run).
- `qrSvg(text, px, quiet? = 2)`: an `<svg>` drawing it in `var(--foreground)`; put it on
  a `qrPlate`, never directly on a dark surface.

## Example: a channel settings page

```ts
import {
  definePage,
  header,
  page,
  settingsForm,
} from "../../../captive-portal/resources/web/ui";

export const mount = definePage((context) => {
  const { lang } = context;
  const form = settingsForm(
    {
      endpoint: "/api/gateway/telegram",
      submit: { zh: "保存并替换通道", en: "Save and replace channel" },
      rows: [
        {
          title: "Bot",
          hint: {
            zh: "在 @BotFather 创建 Bot 后获得 Token",
            en: "Create a bot with @BotFather to get its token",
          },
          fields: [{ kind: "secret", name: "token", label: "Bot Token" }],
        },
      ],
      advanced: {
        fields: [
          {
            kind: "url",
            name: "api_base",
            label: "API Base URL",
            value: "https://api.telegram.org",
          },
          {
            kind: "number",
            name: "draft_min_delta_bytes",
            label: { zh: "草稿最小增量", en: "Draft minimum delta" },
            value: 24,
            unit: "bytes",
          },
        ],
      },
      success: {
        action: {
          label: { zh: "去 Web 聊天试试", en: "Try it in Web chat" },
          run: () => context.navigate("imessage-web"),
        },
      },
    },
    context,
  );
  return page(
    header(
      {
        title: "Telegram",
        lead: {
          zh: "通过 Telegram Bot 收发消息。",
          en: "Send and receive messages through a Telegram bot.",
        },
        icon: new URL("./icon.svg", import.meta.url).href,
        figure: "riffle",
        figureWidth: 280,
      },
      lang,
    ),
    form.element,
  );
});
```

Submitting posts `{"token": "…", "api_base": "https://api.telegram.org",
"draft_min_delta_bytes": 24}` to `/api/gateway/telegram`; a 204 shows
「设备已接受配置」 with the 「去 Web 聊天试试」 action and clears the token.

## Resources a contributor may ship

`tools/web/build.ts` (the plugin's `[tasks.build]`) builds, from `resources/web/`:

| source                     | output in `filesystem/resources/` | manifest field |
| -------------------------- | --------------------------------- | -------------- |
| `entry.ts` (required)      | `entry.js`                        | `module`       |
| `figure.js` or `figure.ts` | `figure.js`                       | `figure`       |
| `icon.svg` or `icon.png`   | copied as is                      | `icon`         |

List every output in the plugin's `plugin.toml` `[tasks.build] outputs`, and
`../captive-portal/resources/web/ui` plus the sources in its `inputs`. An output
whose source is removed is deleted on the next build.

- `icon.svg` is a monochrome mark (fill or stroke in any one colour; the shell
  draws it in `currentColor` at 16px in the sidebar and rows, 18px on phones).
  `icon.png` is a colour image (Inkbox).
- `figure.js` is a live Hairline figure. It uses the shell's global `HL` (the
  kernel) and must not import or bundle it; the build refuses a figure that does.
  It exports the object the design's sources pass to `hairline(...)`:

  ```js
  const { Cam, fit, proj, mk, register, pointer, disposer } = HL;
  function mount({ stage, svg, read }, value) {
    /* … */ return { destroy, set, scan };
  }
  export const figure = {
    name: "router",
    means: "…",
    rules: [1, 3, 7, 8],
    range: [0.5, 1.5, 3],
    mount,
  };
  ```

  The shell imports it once, when an `<hl-figure name="<entry id>">` first needs
  it (an overview tile or the page header), crops it to the design's box for
  `name` (`router`, `socket`, `loupe`, `laptop`; or `figure.view`), feeds it the
  tile's hover zone, and destroys it when the element leaves the page. Reduced
  motion is handled by the kernel.
