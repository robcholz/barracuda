/**
 * The fixed `BARRACUDA …` lines a Board writes to its serial console, which
 * the installer follows after flashing: the boot line from System and the
 * Wi-Fi setup lines from the `wifi` Plugin.
 */

/** One console event the installer acts on. */
export type ConsoleEvent =
  | { kind: "boot"; board: string; version: string }
  | { kind: "hotspot"; ip: string; ssid: string }
  | { kind: "connecting"; ssid: string }
  | { kind: "failed"; reason: string; ssid: string }
  | { kind: "online"; ip: string; port: number };

const MARK = "BARRACUDA ";

/** Removes terminal color codes the log sink may add. */
function plain(line: string): string {
  // eslint-disable-next-line no-control-regex
  return line.replace(/\x1b\[[0-9;]*m/g, "").replace(/\r$/, "");
}

/** `key=value` fields up to `ssid=`, which runs to the end of the line. */
function fields(rest: string): Map<string, string> {
  const result = new Map<string, string>();
  let remaining = rest;
  while (remaining) {
    if (remaining.startsWith("ssid=")) {
      result.set("ssid", remaining.slice(5));
      break;
    }
    const space = remaining.indexOf(" ");
    const field = space < 0 ? remaining : remaining.slice(0, space);
    const equals = field.indexOf("=");
    if (equals > 0) result.set(field.slice(0, equals), field.slice(equals + 1));
    remaining = space < 0 ? "" : remaining.slice(space + 1);
  }
  return result;
}

/** The event a console line reports, or `null` for any other line. */
export function parseConsoleLine(line: string): ConsoleEvent | null {
  const text = plain(line);
  const start = text.indexOf(MARK);
  if (start < 0) return null;
  const values = fields(text.slice(start + MARK.length));
  const get = (key: string) => values.get(key) ?? "";
  if (values.get("system") === "boot")
    return { kind: "boot", board: get("board"), version: get("version") };
  switch (values.get("wifi")) {
    case "hotspot":
      return { kind: "hotspot", ip: get("ip"), ssid: get("ssid") };
    case "connecting":
      return { kind: "connecting", ssid: get("ssid") };
    case "failed":
      return { kind: "failed", reason: get("reason"), ssid: get("ssid") };
    case "online": {
      const port = Number(get("port"));
      return { kind: "online", ip: get("ip"), port: port || 8787 };
    }
    default:
      return null;
  }
}

/** Splits a byte stream into text lines as chunks arrive. */
export class LineReader {
  private decoder = new TextDecoder();
  private pending = "";

  /** The complete lines this chunk finishes. */
  push(chunk: Uint8Array): string[] {
    this.pending += this.decoder.decode(chunk, { stream: true });
    const lines = this.pending.split("\n");
    this.pending = lines.pop() ?? "";
    return lines;
  }
}

/** The setup hotspot's name, from the station MAC as the firmware forms it. */
export function hotspotName(mac: string): string {
  const bytes = mac.split(":");
  if (bytes.length !== 6) return "Barracuda Setup";
  return `Barracuda-${(bytes[4] + bytes[5]).toUpperCase()}`;
}

/** The payload a phone's camera reads to join an open network. */
export function joinNetworkPayload(ssid: string): string {
  const escaped = ssid.replace(/([\\;,:"])/g, "\\$1");
  return `WIFI:T:nopass;S:${escaped};;`;
}

/** The device portal's address once the Board is on the visitor's network. */
export function portalUrl(ip: string, port: number): string {
  return `http://${ip}:${port}/portal/`;
}
