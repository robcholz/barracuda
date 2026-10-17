import {
  button,
  callDevice,
  definePage,
  entryStatus,
  h,
  header,
  kv,
  note,
  page,
  row,
  toastDeviceError,
  type PortalContext,
} from "../../../captive-portal/resources/web/ui";

/** An offer in the Workspace; `incompatible` says why this device cannot install it. */
interface Offer {
  id: string;
  name: string;
  version: string;
  incompatible?: string;
}

/** The install waiting for the person: what it asks for, and the digest the install is pinned to. */
interface Waiting {
  id: string;
  name: string;
  version: string;
  digest: string | null;
  signer: string | null;
  asks: string[];
}

/** `GET /api/apps`. */
interface Listing {
  waiting: Waiting | null;
  offers: Offer[];
}

/**
 * The apps page: the install waiting for the person, with what it asks for and who signed it, and
 * the Workspace's offers. Installing or declining works only from the device's setup hotspot, so
 * only someone at the device decides; asking for an install works from anywhere.
 */
export const mount = definePage((context: PortalContext) => {
  const lang = context.lang;
  const status = entryStatus(context);
  const waiting = h("div");
  const offers = h("div");

  const call = async (
    endpoint: string,
    body: unknown,
    after: () => Promise<void>,
  ) => {
    const result = await callDevice(context, endpoint, {
      method: "POST",
      body,
    });
    if (result.kind === "ok") {
      await after();
    } else if (result.kind !== "aborted") {
      toastDeviceError(context, result);
    }
  };

  const load = async () => {
    const result = await callDevice<Listing>(context, "/api/apps");
    if (result.kind !== "ok" || !result.data) {
      if (result.kind === "error" || result.kind === "offline")
        toastDeviceError(context, result, () => void load());
      return;
    }
    const { waiting: app, offers: list } = result.data;
    void status.refresh();
    waiting.replaceChildren(
      app
        ? row(
            { zh: `安装“${app.name}”？`, en: `Install “${app.name}”?` },
            app.signer
              ? {
                  zh: `版本 ${app.version} · 已签名 \`${app.signer}\``,
                  en: `Version ${app.version} · signed by \`${app.signer}\``,
                }
              : {
                  zh: `版本 ${app.version} · 未签名`,
                  en: `Version ${app.version} · unsigned`,
                },
            lang,
            h(
              "ul",
              { class: "bc-small" },
              (app.asks.length
                ? app.asks
                : [lang === "zh" ? "不需要额外权限" : "No extra permissions"]
              ).map((line) => h("li", {}, line)),
            ),
            h(
              "div",
              { class: "bc-actions" },
              button({ zh: "安装", en: "Install" }, lang, {
                onClick: () =>
                  void call(
                    "/api/apps/install",
                    { id: app.id, digest: app.digest },
                    load,
                  ),
              }),
              button({ zh: "拒绝", en: "Decline" }, lang, {
                variant: "outline",
                onClick: () =>
                  void call("/api/apps/decline", { id: app.id }, load),
              }),
            ),
          )
        : note(
            { zh: "没有等待确认的安装。", en: "No install is waiting." },
            lang,
          ),
    );
    offers.replaceChildren(
      ...(list.length
        ? list.map((offer) =>
            row(
              offer.name,
              offer.incompatible ?? {
                zh: `版本 ${offer.version}`,
                en: `Version ${offer.version}`,
              },
              lang,
              offer.incompatible
                ? null
                : h(
                    "div",
                    {},
                    button({ zh: "请求安装", en: "Ask to install" }, lang, {
                      variant: "outline",
                      size: "sm",
                      onClick: () =>
                        void call("/api/apps/request", { id: offer.id }, load),
                    }),
                  ),
            ),
          )
        : [
            note(
              {
                zh: "工作区里没有可安装的应用。",
                en: "The Workspace offers no apps.",
              },
              lang,
            ),
          ]),
    );
  };
  void load();

  return page(
    header(
      {
        title: { zh: "应用", en: "Apps" },
        lead: {
          zh: "安装工作区里的应用。只有连在设备热点上的手机或电脑能确认安装。",
          en: "Install the apps the Workspace offers. Only a phone or computer on the device's hotspot can confirm an install.",
        },
        extra: kv([[{ zh: "状态", en: "Status" }, status.slot]], lang, {
          live: true,
        }),
      },
      lang,
    ),
    h("h2", { class: "bc-title" }, lang === "zh" ? "等待确认" : "Waiting"),
    waiting,
    h("h2", { class: "bc-title" }, lang === "zh" ? "工作区" : "Workspace"),
    offers,
  );
});
