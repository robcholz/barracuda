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
| `has(id)`         | whether the manifest lists an entry with that ID now; offer a way to another plugin's page only when it does |
| `status(id?)`     | the entry's latest `GET /portal/status` record (this page's own by default), or `null`     |
| `refreshStatus()` | reads the status again and redraws the sidebar and overview; call it after a save succeeds |
| `badge?(badge)`   | shows the page's live state in the top bar (`{ label, tone: "live" \| "lost" }`), or clears it with `null`; Web chat's connection |

`mount` may return a cleanup function (or a promise of one). After the signal
aborts, `toast`, `navigate`, `refreshStatus` and `badge` do nothing.

An entry is listed only while its plugin is enabled, and a page depends on no
other plugin's page. Before offering to go to another entry (a toast action, a
button), ask `context.has(id)`, and leave the offer out when it answers `false`;
`navigate` to an absent entry shows the shell's 「插件已停用」 state. Types: `PortalModule`,
`PortalContext`, `PageBadge`, `Toast`, `Lang`, `PortalText`, `EntryStatus`, `EntryState` (all
exported by the kit).

The device serves only four connections at once and refuses the rest, so the shell routes the
window's `fetch` through one queue (two requests in flight) and retries a refused `GET`. Pages
use plain `fetch`; `assetIcon` fetches its file through the same queue.

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
- `icon(ICON_*, size?, className?)`: a Lucide icon (`ICON_EYE`, `ICON_REFRESH`, `ICON_PLUS`,
  `ICON_WIFI`, `ICON_COPY`, `ICON_EXTERNAL_LINK`, `ICON_MAIL`, `ICON_KEY_ROUND`, … from
  `icons.ts`), 16px by default, 18px on phone rows; `className` adds classes such as a
  text colour (`bc-muted`, `bc-warning`).
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
- `kv(rows, lang, { live?, mono? })`: a `.bc-kv` table (96px keys); `live` makes it
  a polite status region.
- `term(label, tip, lang, { start? })`: the dotted-underline aside (`明文`, `名额`).
- `entryStatus(context)`: the open entry's 「状态」 badge from `/portal/status` (已配置 / 未配置) as
  `{ slot, refresh }`, for a write-only form's header `kv`; call `refresh` after a save.
- `badge(label, lang, { signal? })`, `button(label, lang, { variant, size, icon, type, onClick })`,
  `frame(...children)`, `row(title, hint, lang, ...children)` (a label-left row; `backticks` in the
  hint mark machine values, set in mono: 「在 `@BotFather` 发送 `/newbot` 获得 Token」).
- `monoParts(text)`: the same backtick split, for any line that mixes words and machine values.

Settings form (`settings.ts`)

- `settingsForm(options, context)` returns `{ element, values(), setError(name, message, code?), submit(), footer }`.
  - `options.rows`: `{ title, hint?, link?, fields, blocks? }[]`, each a label-left row;
    `link: { label, href }` is an external link under the hint (「打开 @BotFather」, new tab);
    `blocks` are nodes after the fields in the row body (a `resultCard`, a `qrLink`).
  - `options.advanced`: `{ hint?, fields, open?, columns?, onToggle? }`, the 「高级」 fold
    (hint defaults to 「已填入默认值」); it opens by itself when one of its fields is invalid.
    `onToggle(open)` runs whenever it opens or closes (WeChat shows its footer only while open).
  - `options.endpoint`: the POST target. It is never shown: the footer holds only
    「清空」 (restores defaults) and the submit button.
  - `options.submit`: the button label, a verb naming the result.
  - `options.body(values)`: shapes the JSON body (default: the values object;
    wrap it, for example `(v) => [v]`, when the endpoint takes a batch).
  - `options.success`: overrides the success toast (`title`, `body`, `action`).
  - `options.onSuccess(values)`: runs after a 2xx.
  - `options.onError(error)`: runs on a non-2xx with the device's `DeviceError`
    (`{ status, error, message?, code? }`, see below) before the toast; return
    `{ title?, body? }` to replace the toast's, typically `{ body: error.message }`, and
    call `setError` to put an upstream refusal on its field.
  - `setError(name, message, code?)`: marks a field; the message says what to do and ends in
    「。」, `code` follows it in mono (「检查 App ID 和 App Secret。 · `10004`」). `footer` is the footer element, for pages
    that show it only in some states (hide it with `style.display = "none"`).
- Fields, all with `name`, `label`, optional `hint` and `optional`:
  - `{ kind: "text" | "url", value?, placeholder?, mono? }` — trimmed; `url` must be http(s);
  - `{ kind: "secret", placeholder? }` — password input with 显示/隐藏, never prefilled,
    cleared after the device accepts it and when the page goes away;
  - `{ kind: "number", value?, unit?, min? = 0, max? = 4294967295 }` — integer, unit in an addon;
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
  「未收到设备确认」 with 「重试」 when `retry` is given (no disclaimer line).
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

Blocks (`blocks.ts`): the design's form blocks, for a row's `blocks` or anywhere

- `resultCard({ title, badge?, live?, sub?, initial?, rows?, action? }, lang)`: what a check found
  (`role="status"`), a `.bc-card` with one `.bc-card__body`: a 40px tile with `initial`, or without
  one a bare `check` in `success` on the title's line (never boxed: the tile holds only an initial),
  the title, `sub` in mono, the badge (neutral, beside an initial: 「已验证」; the signal chip only
  with `live`, a live connection such as BlueBubbles' 「已连接」; leave it out for a plain state such
  as 「微信已绑定」, which the check and the title already say), a `.bc-kv` of `rows` (values mono
  unless the row's third item is `false`) and one `action` node.
- `stepList(steps, { current, done }, lang)`: `.bc-steps`, numbered `radius-sm` marks with mono
  numbers (`01`); the first `done` (`.bc-step--done`) show a check in `success`, `current`
  (`aria-current="step"`, or `-1` for none) fills `muted`.
- `note(text, lang, { mono?, action?: { label, onClick } })`: a small muted line with a
  mono value (muted too) and a `button.bc-link` after a dot (「验证码已发到 `you@example.com` · 重新发送」).
- `qrPlate(data, px, lang, { dim? })`: a QR Code on a `.bc-qr` plate that stays light
  in the dark theme (`data-theme="light"`); `data: null` is the empty plate while a code
  loads; `dim` (`.bc-qr--dim`) fades the code under a `.bc-qr__overlay` label (「已扫码」,
  「二维码已过期」). Put it in a `.bc-card__body`, never on its own.
- `qrLink(url, label, lang, { px? = 120 })`: a link to open on a phone, as a `.bc-card`: its
  code, the URL in mono and an outline button that opens it in a new tab.
- `copyText(text)`: copies to the clipboard, falling back to a selection copy where
  `navigator.clipboard` is missing (plain HTTP); resolves whether it worked.

QR Codes (`qr.ts`)

- `encodeQr(text, mask?)`: byte mode (UTF-8), level M, versions 1–40, the smallest that
  fits (throws a `RangeError` past 2331 bytes); returns `{ version, mask, size, modules }`
  with `modules[y * size + x]` 1 for dark. Masks are chosen by the standard's penalty rules.
- `qrPath(code, quiet? = 2)`: the dark modules as one SVG path (one rectangle per run).
- `qrSvg(text, px, quiet? = 2)`: an `<svg>` of it; put it directly in a `.bc-qr` plate
  (`qrPlate`), which fills it `foreground` on light, never on a dark surface.

## Example: a settings page

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
      endpoint: "/api/example",
      submit: { zh: "保存", en: "Save" },
      rows: [
        {
          title: { zh: "服务", en: "Service" },
          hint: {
            zh: "在服务的控制台创建 `API Key`",
            en: "Create an `API Key` in the service's console",
          },
          fields: [{ kind: "secret", name: "api_key", label: "API Key" }],
        },
      ],
      advanced: {
        fields: [
          {
            kind: "url",
            name: "api_base",
            label: "API Base URL",
            value: "https://api.example.com",
          },
          {
            kind: "number",
            name: "timeout_secs",
            label: { zh: "超时", en: "Timeout" },
            value: 30,
            unit: "s",
          },
        ],
      },
      // another plugin's page: offered only while the manifest lists it
      success: context.has("other")
        ? {
            action: {
              label: { zh: "去看看", en: "Take a look" },
              run: () => context.navigate("other"),
            },
          }
        : undefined,
    },
    context,
  );
  return page(
    header(
      {
        title: "Example",
        lead: {
          zh: "连接一个外部服务。",
          en: "Connect an external service.",
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

Submitting posts `{"api_key": "…", "api_base": "https://api.example.com",
"timeout_secs": 30}` to `/api/example`; a 204 shows 「设备已接受配置」, with the
「去看看」 action while the manifest lists `other`, and clears the key.

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
