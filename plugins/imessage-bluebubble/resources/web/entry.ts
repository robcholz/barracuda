import {
  button,
  channelInbound,
  configuredRow,
  definePage,
  h,
  header,
  note,
  page,
  readChannel,
  resultCard,
  settingsForm,
  type ChannelStatus,
} from "../../../captive-portal/resources/web/ui";

const T = {
  zh: {
    lead: "连接 BlueBubbles 服务器以接入 iMessage。",
    submit: "保存并替换通道",
    server: "服务器",
    serverHint: "BlueBubbles Server 的「设置 → API」里有地址和密码",
    url: "服务器 URL",
    password: "服务器密码",
    test: "测试连接",
    connected: "已连接",
    version: "服务器版本",
    enabled: "已启用",
    disabled: "未启用",
    rejected: "检查服务器 URL 和密码后重新测试。",
    unreachable: "连不上这台服务器，连接未测试",
    options: "选项",
    optionsHint: "需要服务器已启用 Private API",
    matched: "已按服务器设置开启",
    privateApi: "使用 Private API",
    editDelta: "流式编辑最小增量",
    maxEdits: "最大流式编辑次数",
    tryChat: "去 Web 聊天试试",
    how: "用 iMessage 给这台 Mac 发送",
  },
  en: {
    lead: "Connect a BlueBubbles server to reach iMessage.",
    submit: "Save and replace channel",
    server: "Server",
    serverHint: "Find both in BlueBubbles Server under Settings → API",
    url: "Server URL",
    password: "Server password",
    test: "Test connection",
    connected: "Connected",
    version: "Server",
    enabled: "Enabled",
    disabled: "Disabled",
    rejected: "Check the server URL and password, then test again.",
    unreachable: "Couldn't reach the server; the connection is not tested",
    options: "Options",
    optionsHint: "Needs the Private API enabled on the server",
    matched: "Set to match the server",
    privateApi: "Use the Private API",
    editDelta: "Streaming edit minimum delta",
    maxEdits: "Max streaming edits",
    tryChat: "Try it in Web chat",
    how: "From iMessage, send this Mac",
  },
};

/** The parts of BlueBubbles Server's `GET /api/v1/server/info` reply the page reads. */
interface ServerInfo {
  message?: string;
  data?: {
    server_version?: string;
    os_version?: string;
    private_api?: boolean;
  };
}

const SAMPLE_URL = "https://bluebubbles.example.com";
const ENDPOINT = "/api/gateway/bluebubbles";

/**
 * The BlueBubbles page: the server URL and password, checked in the browser against the server's
 * `GET /api/v1/server/info`, then saved with `POST /api/gateway/bluebubbles`. `GET /status` below
 * that path says whether a channel is configured, and its mode and allowed accounts (`/mode`, `/owners`) show
 * while it is.
 */
export const mount = definePage((context) => {
  const { lang } = context;
  const t = T[lang];
  const result = h("div", { style: { display: "contents" } });
  const test = button(t.test, lang, {
    variant: "outline",
    onClick: () => void check(),
  });
  const current = configuredRow("BlueBubbles", lang);
  const inbound = channelInbound(context, {
    endpoint: ENDPOINT,
    channel: "BlueBubbles",
    how: t.how,
  });
  const form = settingsForm(
    {
      endpoint: ENDPOINT,
      submit: t.submit,
      rows: [
        {
          title: t.server,
          hint: t.serverHint,
          fields: [
            {
              kind: "url",
              name: "server_url",
              label: t.url,
              placeholder: SAMPLE_URL,
            },
            {
              kind: "secret",
              name: "password",
              label: t.password,
              action: test,
            },
          ],
          blocks: result,
        },
        {
          title: t.options,
          hint: t.optionsHint,
          fields: [
            {
              kind: "switch",
              name: "use_private_api",
              label: t.privateApi,
              value: true,
            },
          ],
        },
      ],
      advanced: {
        open: true,
        fields: [
          {
            kind: "number",
            name: "stream_edit_min_delta_bytes",
            label: t.editDelta,
            value: 128,
            unit: "bytes",
          },
          {
            kind: "number",
            name: "stream_max_edits",
            label: t.maxEdits,
            value: 4,
          },
        ],
      },
      onError: (error) => ({ body: error.message }),
      onSuccess: () => {
        current.show(true);
        void inbound.refresh();
        void context.refreshStatus();
      },
      success: {
        action: {
          label: t.tryChat,
          run: () => context.navigate("imessage-web"),
        },
      },
    },
    context,
  );
  form.element.prepend(current.element);
  inbound.attach(form);
  void readChannel<ChannelStatus>(context, ENDPOINT).then((state) => {
    if (state?.configured) current.show(true);
    inbound.apply(state);
  });
  const field = (name: string) =>
    form.element.querySelector<HTMLElement>(`[name="${name}"]`)!;
  const privateApi = field("use_private_api");
  const optionsHint = privateApi
    .closest(".bc-row")!
    .querySelector<HTMLElement>(".bc-row__label .bc-muted")!;

  let run = 0;
  const clear = () => {
    run++;
    result.replaceChildren();
    optionsHint.textContent = t.optionsHint;
  };
  field("server_url").addEventListener("input", clear);
  field("password").addEventListener("input", clear);
  form.element.addEventListener("reset", clear);

  async function check() {
    const values = form.values();
    if (!values) return;
    clear();
    const mine = run;
    const base = String(values.server_url).replace(/\/+$/, "");
    const request = new AbortController();
    const cancel = () => request.abort();
    context.signal.addEventListener("abort", cancel, { once: true });
    const timeout = setTimeout(cancel, 10_000);
    test.disabled = true;
    try {
      const response = await fetch(
        `${base}/api/v1/server/info?password=${encodeURIComponent(String(values.password))}`,
        { signal: request.signal, cache: "no-store", credentials: "omit" },
      );
      let reply: ServerInfo = {};
      try {
        reply = (await response.json()) as ServerInfo;
      } catch (error) {
        // an error page is still an answer from the server; a 2xx that isn't JSON is not BlueBubbles
        if (response.ok) throw error;
      }
      if (mine !== run || context.signal.aborted) return;
      const info = reply.data;
      if (!response.ok || !info) {
        // the field says what to do; the server's status and words follow in mono
        form.setError(
          "server_url",
          t.rejected,
          [response.status, reply.message ?? response.statusText]
            .filter(Boolean)
            .join(" "),
        );
        return;
      }
      const rows: [string, string, boolean?][] = [];
      if (info.server_version) rows.push([t.version, info.server_version]);
      if (info.os_version) rows.push(["macOS", info.os_version]);
      if (typeof info.private_api === "boolean") {
        rows.push([
          "Private API",
          info.private_api ? t.enabled : t.disabled,
          false,
        ]);
        privateApi.setAttribute("aria-checked", String(info.private_api));
        if (info.private_api) optionsHint.textContent = t.matched;
      }
      result.replaceChildren(
        resultCard(
          {
            title: "BlueBubbles Server",
            badge: t.connected,
            live: true,
            sub: base,
            rows,
          },
          lang,
        ),
      );
    } catch {
      // no route from this browser, a CORS refusal or not a BlueBubbles server: saving still works
      if (mine === run && !context.signal.aborted)
        result.replaceChildren(note(t.unreachable, lang));
    } finally {
      clearTimeout(timeout);
      context.signal.removeEventListener("abort", cancel);
      if (!context.signal.aborted) test.disabled = false;
    }
  }

  return page(
    header(
      {
        title: "BlueBubbles",
        lead: t.lead,
        icon: new URL("./icon.svg", import.meta.url).href,
        figure: "riffle",
        figureWidth: 280,
      },
      lang,
    ),
    inbound.alert,
    form.element,
  );
});
