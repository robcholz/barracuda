/**
 * The channel page kit: bundled at build time into each channel's portal entry, beside the portal
 * UI kit, for the JSON the Gateway serves under every channel's config path. Documented in
 * docs/develop/web/channel-page-kit.mdx.
 */
export {
  configuredRow,
  readChannel,
  type ChannelState,
  type ConfiguredRow,
} from "./channel";
export {
  CHANNEL_MODES,
  accountsRow,
  channelInbound,
  modeRow,
  type AccountsRow,
  type AccountsRowOptions,
  type ChannelInbound,
  type ChannelInboundOptions,
  type ChannelMode,
  type ChannelStatus,
  type ModeRow,
  type ModeRowOptions,
  type Owner,
  type OwnersReply,
  type ReceiveState,
} from "./inbound";
