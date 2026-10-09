import type { Lang, PortalContext } from "../src/contract";
import { resultCard } from "./blocks";
import { callDevice } from "./device";
import type { Text } from "./dom";
import { row } from "./layout";
import { KIT_STRINGS } from "./strings";

/**
 * What `GET <endpoint>/status` answers: whether a channel is stored and, while it is, `config`, the
 * stored settings without their secrets (each channel's own fields).
 */
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
  /** Shows or hides the row; `sub` is the stored account in mono under the name (an App ID, a URL). */
  show(visible: boolean, sub?: string): void;
}

/**
 * The configured state of a channel page: a 「通道」 row holding the result card 「<name>」 with the
 * 已配置 badge and the stored account under it, placed above the form that replaces it. Hidden
 * until `show(true)`.
 */
export function configuredRow(name: Text, lang: Lang): ConfiguredRow {
  const s = KIT_STRINGS[lang];
  const element = row(s.channel, undefined, lang);
  const body = element.querySelector(".bc-row__body")!;
  // the card exists only while shown, so a hidden row holds no status region
  const show = (visible: boolean, sub?: string) => {
    element.hidden = !visible;
    body.replaceChildren(
      ...(visible
        ? [resultCard({ title: name, badge: s.configured, sub }, lang)]
        : []),
    );
  };
  show(false);
  return { element, show };
}
