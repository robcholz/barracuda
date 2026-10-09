/**
 * The portal UI kit: bundled into each contributor at build time, never loaded by the shell at
 * runtime. Documented in ./README.md.
 */
export type {
  Cleanup,
  EntryState,
  EntryStatus,
  FigureDefinition,
  FigureHandle,
  FigureTarget,
  Lang,
  PortalContext,
  PortalModule,
  PortalText,
  Toast,
} from "../src/contract";
export {
  append,
  assetIcon,
  h,
  icon,
  mark,
  pick,
  twoDigits,
  type Child,
  type Children,
  type Props,
  type Text,
} from "./dom";
export {
  badge,
  button,
  definePage,
  frame,
  header,
  kv,
  page,
  row,
  term,
  type ButtonOptions,
  type HeaderOptions,
} from "./layout";
export {
  fieldControl,
  settingsForm,
  submitJson,
  type Field,
  type NumberField,
  type RadioField,
  type SecretField,
  type SelectField,
  type SettingsForm,
  type SettingsFormOptions,
  type SettingsRow,
  type SubmitOptions,
  type SubmitOutcome,
  type SwitchField,
  type TextField,
  type Values,
} from "./settings";
export { KIT_STRINGS } from "./strings";
export { fitColumns } from "./grid";
export {
  copyText,
  note,
  qrLink,
  qrPlate,
  resultCard,
  stepList,
  type ResultCardOptions,
} from "./blocks";
export {
  callDevice,
  deviceError,
  toastDeviceError,
  type DeviceError,
  type DeviceRequest,
  type DeviceResult,
} from "./device";
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
export { encodeQr, qrPath, qrSvg, type QrCode } from "./qr";
export * from "./icons";
export * from "./marks";
