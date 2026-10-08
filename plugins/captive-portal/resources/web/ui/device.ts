import type { PortalContext } from "../src/contract";
import { KIT_STRINGS } from "./strings";

/**
 * A non-2xx reply from the device. Its JSON body is `{"error": kind, "message"?, "code"?}`; `message`
 * and `code` are the upstream service's own words when it gave any.
 */
export interface DeviceError {
  status: number;
  /** `invalid_request`, `verification_failed`, `upstream_unavailable`, `conflict`, …; empty when the body had none. */
  error: string;
  message?: string;
  code?: string;
}

/** Reads a failed response's `{error, message, code}` body; a missing or malformed body leaves only the status. */
export async function deviceError(response: Response): Promise<DeviceError> {
  const out: DeviceError = { status: response.status, error: "" };
  try {
    const body: unknown = await response.json();
    if (body && typeof body === "object") {
      const { error, message, code } = body as Record<string, unknown>;
      if (typeof error === "string") out.error = error;
      if (typeof message === "string" && message) out.message = message;
      if ((typeof code === "string" && code) || typeof code === "number")
        out.code = String(code);
    }
  } catch {
    // not JSON: the status says enough
  }
  return out;
}

export type DeviceResult<T> =
  | { kind: "ok"; status: number; data: T | null }
  | { kind: "error"; error: DeviceError }
  /** No reply: a network error or the timeout. */
  | { kind: "offline" }
  /** The page went away first; report nothing. */
  | { kind: "aborted" };

export interface DeviceRequest {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  /** Sent as JSON when present. */
  body?: unknown;
  /** Default 15 s. */
  timeoutMs?: number;
}

/**
 * Calls a device endpoint (`redirect: "error"`, `cache: "no-store"`, aborted with the page or after
 * the timeout) and reads a JSON reply. Reports nothing itself: pass a failure to
 * {@link toastDeviceError}, or show it inline.
 */
export async function callDevice<T = unknown>(
  context: PortalContext,
  endpoint: string,
  options: DeviceRequest = {},
): Promise<DeviceResult<T>> {
  const request = new AbortController();
  const cancel = () => request.abort();
  context.signal.addEventListener("abort", cancel, { once: true });
  const timeout = setTimeout(cancel, options.timeoutMs ?? 15_000);
  try {
    const response = await fetch(endpoint, {
      method: options.method ?? "GET",
      headers:
        options.body === undefined
          ? undefined
          : { "Content-Type": "application/json" },
      body:
        options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: request.signal,
      cache: "no-store",
      redirect: "error",
    });
    if (!response.ok) {
      const error = await deviceError(response);
      return context.signal.aborted
        ? { kind: "aborted" }
        : { kind: "error", error };
    }
    let data: T | null = null;
    try {
      data = response.status === 204 ? null : ((await response.json()) as T);
    } catch {
      data = null;
    }
    return context.signal.aborted
      ? { kind: "aborted" }
      : { kind: "ok", status: response.status, data };
  } catch {
    return context.signal.aborted ? { kind: "aborted" } : { kind: "offline" };
  } finally {
    clearTimeout(timeout);
    context.signal.removeEventListener("abort", cancel);
  }
}

/**
 * Toasts a failed device call in the kit's words: 「配置被拒绝」 for 4xx (「接口不可用」 for 404),
 * 「提交失败」 otherwise, with the status (and upstream code) in mono and the device's `message`
 * as the body; 「未收到设备确认」 when nothing came back.
 */
export function toastDeviceError(
  context: PortalContext,
  result: { kind: "error"; error: DeviceError } | { kind: "offline" },
  retry?: () => void,
) {
  const s = KIT_STRINGS[context.lang];
  if (result.kind === "offline") {
    context.toast({
      kind: "error",
      title: s.noReply,
      body: s.noReplyBody,
      action: retry && { label: s.retry, run: retry },
    });
    return;
  }
  const { status, message, code } = result.error;
  context.toast({
    kind: "error",
    title:
      status === 404
        ? s.missing
        : status >= 400 && status < 500
          ? s.rejected
          : s.failed,
    body: message,
    code: code ? `${status} · ${code}` : String(status),
  });
}
