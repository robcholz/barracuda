/**
 * The portal UI kit: bundled into each contributor at build time, never loaded by the shell at
 * runtime. Documented in docs/plugins/captive-portal/ui-kit.mdx.
 */
export type {
  Cleanup,
  EntryState,
  EntryStatus,
  FigureDefinition,
  FigureHandle,
  FigureTarget,
  Lang,
  PageBadge,
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
  entryStatus,
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
export { encodeQr, qrPath, qrSvg, type QrCode } from "./qr";
export * from "./icons";
export * from "./marks";
