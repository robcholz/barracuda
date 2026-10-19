import {
  ICON_ARROW_RIGHT,
  ICON_BOT,
  ICON_CHECK,
  ICON_CHEVRON_DOWN,
  ICON_CHEVRON_RIGHT,
  ICON_CIRCLE_ALERT,
  ICON_CIRCLE_CHECK,
  ICON_CIRCLE_HELP,
  ICON_CLOCK,
  ICON_CODE,
  ICON_COPY,
  ICON_CPU,
  ICON_DOWNLOAD,
  ICON_EXTERNAL_LINK,
  ICON_GITHUB,
  ICON_GRADUATION_CAP,
  ICON_LANGUAGES,
  ICON_LAYOUT_GRID,
  ICON_LIGHTBULB,
  ICON_LOADER_CIRCLE,
  ICON_MESSAGE_SQUARE,
  ICON_MONITOR,
  ICON_MOON,
  ICON_PLUS,
  ICON_SEARCH,
  ICON_SUN,
  ICON_TERMINAL,
  ICON_UPLOAD,
  ICON_USB,
  ICON_WIFI,
  ICON_X,
} from "../../../design-system/icons";

/** The design system's Lucide icons (design-system/icons.ts) by the names the site's pages use. */
const ICONS = {
  x: ICON_X,
  languages: ICON_LANGUAGES,
  chevronDown: ICON_CHEVRON_DOWN,
  chevronRight: ICON_CHEVRON_RIGHT,
  check: ICON_CHECK,
  github: ICON_GITHUB,
  copy: ICON_COPY,
  terminal: ICON_TERMINAL,
  code: ICON_CODE,
  lightbulb: ICON_LIGHTBULB,
  graduationCap: ICON_GRADUATION_CAP,
  cpu: ICON_CPU,
  arrowRight: ICON_ARROW_RIGHT,
  search: ICON_SEARCH,
  wifi: ICON_WIFI,
  bot: ICON_BOT,
  chat: ICON_MESSAGE_SQUARE,
  clock: ICON_CLOCK,
  apps: ICON_LAYOUT_GRID,
  help: ICON_CIRCLE_HELP,
  sun: ICON_SUN,
  moon: ICON_MOON,
  monitor: ICON_MONITOR,
  download: ICON_DOWNLOAD,
  upload: ICON_UPLOAD,
  usb: ICON_USB,
  external: ICON_EXTERNAL_LINK,
  alert: ICON_CIRCLE_ALERT,
  spinner: ICON_LOADER_CIRCLE,
  pending: ICON_CLOCK,
  circleCheck: ICON_CIRCLE_CHECK,
  plus: ICON_PLUS,
};

export type IconName = keyof typeof ICONS;

/** An icon, drawn at 16px with a 2px stroke by the design system's `.bc-icon`. */
export function Icon({
  name,
  className,
}: {
  name: IconName;
  className?: string;
}) {
  return (
    <svg
      className={className ? `bc-icon ${className}` : "bc-icon"}
      viewBox="0 0 24 24"
      aria-hidden="true"
      dangerouslySetInnerHTML={{ __html: ICONS[name] }}
    />
  );
}
