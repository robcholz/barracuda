import {
  callDevice,
  resultCard,
  row,
  type Lang,
  type PortalContext,
  type Text,
} from "../../../../captive-portal/resources/web/ui";

/** The channel kit's own copy; `inbound.ts` keeps the rows' strings beside them. */
const STRINGS = {
  zh: { channel: "通道" },
  en: { channel: "Channel" },
} satisfies Record<Lang, unknown>;

/** What `GET <endpoint>/status` answers: whether a channel is stored, never the settings. */
export interface ChannelState {
  configured: boolean;
}

/**
 * Reads `GET <endpoint>/status` (`{"configured": bool, …}`) for a channel's config path `endpoint`,
 * resolving the reply, or `null` when the device gave none in that shape or the page went away. It
 * never toasts: the page simply shows no state.
 */
export async function readChannel<T extends object = object>(
  context: PortalContext,
  endpoint: string,
): Promise<(ChannelState & T) | null> {
  const result = await callDevice<ChannelState & T>(
    context,
    `${endpoint}/status`,
  );
  return result.kind === "ok" && typeof result.data?.configured === "boolean"
    ? result.data
    : null;
}

export interface ConfiguredRow {
  /** A label-left 「通道」 row; hidden until {@link ConfiguredRow.show}. */
  element: HTMLElement;
  show(visible: boolean): void;
}

/**
 * The configured state of a channel page: a 「通道」 row holding the result card 「<name>」 led by
 * the bare `success` check, placed above the form that replaces it. Hidden until `show(true)`.
 */
export function configuredRow(name: Text, lang: Lang): ConfiguredRow {
  const element = row(STRINGS[lang].channel, undefined, lang);
  const body = element.querySelector(".bc-row__body")!;
  // the card exists only while shown, so a hidden row holds no status region
  const show = (visible: boolean) => {
    element.hidden = !visible;
    body.replaceChildren(
      ...(visible ? [resultCard({ title: name }, lang)] : []),
    );
  };
  show(false);
  return { element, show };
}
