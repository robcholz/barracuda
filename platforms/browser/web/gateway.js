const GATEWAY_SEGMENT = "_barracuda";

export function gatewayUrl(port, path, base = location.href) {
  requirePort(port);
  const targetPath = normalizePath(path);
  return new URL(`${GATEWAY_SEGMENT}/${port}${targetPath}`, base);
}

export function parseGatewayUrl(url, scope) {
  const value = url instanceof URL ? url : new URL(url);
  const root = new URL(scope);
  if (value.origin !== root.origin) return undefined;
  const prefix = `${root.pathname}${GATEWAY_SEGMENT}/`;
  if (!value.pathname.startsWith(prefix)) return undefined;
  const remainder = value.pathname.slice(prefix.length);
  const slash = remainder.indexOf("/");
  const portText = slash < 0 ? remainder : remainder.slice(0, slash);
  const port = Number(portText);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) return undefined;
  const pathname = slash < 0 ? "/" : remainder.slice(slash);
  return { port, pathname };
}

function requirePort(port) {
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new RangeError("System service port must be between 1 and 65535");
  }
}

function normalizePath(path) {
  if (typeof path !== "string" || path.length === 0) return "/";
  return path.startsWith("/") ? path : `/${path}`;
}
