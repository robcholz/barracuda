/**
 * The page's own copy of each saved session's conversation, kept in `localStorage`, so a reload or
 * a switch back to the session draws what was said instead of an empty log. The device keeps the
 * session's real history; this copy only redraws the page and is never sent back.
 *
 * A session's copy is the frames that drew it, in order: the messages sent from this browser and
 * the bridge's message events. Consecutive text deltas of one message are joined as they arrive,
 * so a long reply is one frame. A copy past {@link MAX_CHARS} loses its oldest turns, and only the
 * {@link MAX_SESSIONS} sessions saved last are kept. Storage that is full, blocked or missing
 * leaves the page as it was: without a copy.
 */

/** A message sent from this browser: its id, text, quoted message and files' name, type and size. */
export type SentFrame = [
  "u",
  string,
  string,
  { id: string; text: string } | null,
  [string, string, number][],
];
/** A bridge event (`message.*`) as the page received it. */
export type EventFrame = ["e", string, Record<string, unknown>];
export type Frame = SentFrame | EventFrame;

export const PREFIX = "barracuda.chat.";
/** UTF-16 code units one session's copy may take. */
export const MAX_CHARS = 100_000;
export const MAX_SESSIONS = 20;

interface Saved {
  at: number;
  frames: Frame[];
}

function storage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function read(store: Storage, key: string): Saved | null {
  try {
    const saved = JSON.parse(store.getItem(key) ?? "null") as Saved | null;
    return saved && Array.isArray(saved.frames) ? saved : null;
  } catch {
    return null;
  }
}

function keys(store: Storage) {
  const found: string[] = [];
  for (let index = 0; index < store.length; index++) {
    const key = store.key(index);
    if (key?.startsWith(PREFIX)) found.push(key);
  }
  return found;
}

/** The frames saved for `session`, oldest first; none when there is no readable copy. */
export function load(session: string): Frame[] {
  const store = storage();
  return (store && read(store, PREFIX + session)?.frames) || [];
}

/** Adds `frame`, joining a text delta to the one before it when both belong to one stream. */
export function append(frames: Frame[], frame: Frame) {
  const last = frames[frames.length - 1];
  if (frame[0] === "e" && last?.[0] === "e" && last[1] === frame[1]) {
    const [, event, data] = frame;
    const before = last[2];
    if (before.message_id === data.message_id) {
      if (event === "message.delta" && typeof data.delta === "string") {
        before.delta = String(before.delta ?? "") + data.delta;
        return;
      }
      const text = (data.payload as Record<string, unknown> | null)?.text;
      const prior = before.payload as Record<string, unknown> | null;
      if (
        event === "message.event" &&
        typeof data.type === "string" &&
        data.type.endsWith("_delta") &&
        data.type === before.type &&
        typeof text === "string" &&
        typeof prior?.text === "string"
      ) {
        before.payload = { ...prior, text: prior.text + text };
        return;
      }
    }
  }
  frames.push(frame);
}

/** Cuts the frames from the message `id` was sent as, for a message taken back. */
export function cut(frames: Frame[], id: string) {
  for (let index = frames.length - 1; index >= 0; index--) {
    const frame = frames[index];
    if (frame[0] === "u" && frame[1] === id) {
      frames.length = index;
      return;
    }
  }
}

/** Drops the oldest turns, each from a sent message to the next, until the copy fits. */
export function trim(frames: Frame[], max = MAX_CHARS) {
  const sizes = frames.map((frame) => JSON.stringify(frame).length);
  let total = sizes.reduce((sum, size) => sum + size, 0);
  let start = 0;
  while (total > max && start < frames.length) {
    let next = start + 1;
    while (next < frames.length && frames[next][0] !== "u") next++;
    for (let index = start; index < next; index++) total -= sizes[index];
    start = next;
  }
  if (start) frames.splice(0, start);
}

/** Saves `session`'s copy, trimmed to fit, keeping the {@link MAX_SESSIONS} saved last. */
export function save(session: string, frames: Frame[], now = Date.now()) {
  const store = storage();
  if (!store) return;
  trim(frames);
  const key = PREFIX + session;
  if (!frames.length) return forget([session]);
  const value = JSON.stringify({ at: now, frames } satisfies Saved);
  const others = keys(store)
    .filter((other) => other !== key)
    .map((other) => ({ key: other, at: read(store, other)?.at ?? 0 }))
    .sort((a, b) => b.at - a.at);
  for (const old of others.slice(MAX_SESSIONS - 1)) store.removeItem(old.key);
  // a full store gives up the oldest other copies first, then this one
  for (;;) {
    try {
      store.setItem(key, value);
      return;
    } catch {
      const oldest = others.pop();
      if (!oldest) return;
      store.removeItem(oldest.key);
    }
  }
}

/** Removes the copies of `sessions`. */
export function forget(sessions: Iterable<string>) {
  const store = storage();
  if (!store) return;
  for (const session of sessions)
    try {
      store.removeItem(PREFIX + session);
    } catch {
      // nothing to remove
    }
}

/** Removes every copy whose session the device no longer lists. */
export function prune(listed: Set<string>) {
  const store = storage();
  if (!store) return;
  forget(
    keys(store)
      .map((key) => key.slice(PREFIX.length))
      .filter((session) => !listed.has(session)),
  );
}
