/**
 * A zip of an app's directory, for the download when the device portal cannot be reached. Files
 * are stored, not compressed: an app is small, and a stored zip opens everywhere.
 */

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

/** CRC-32 as zip uses it. */
export function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (const byte of bytes) crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

/** Zips `files` into the folder `folder/`. */
export function zip(
  folder: string,
  files: ReadonlyMap<string, Uint8Array>,
): Uint8Array<ArrayBuffer> {
  const utf8 = new TextEncoder();
  const entries = [...files].map(([name, bytes]) => ({
    name: utf8.encode(`${folder}/${name}`),
    bytes,
    crc: crc32(bytes),
  }));
  const local = entries.reduce(
    (sum, entry) => sum + 30 + entry.name.length + entry.bytes.length,
    0,
  );
  const central = entries.reduce(
    (sum, entry) => sum + 46 + entry.name.length,
    0,
  );
  const out = new Uint8Array(local + central + 22);
  const view = new DataView(out.buffer);
  let at = 0;
  const offsets: number[] = [];
  for (const entry of entries) {
    offsets.push(at);
    view.setUint32(at, 0x04034b50, true);
    view.setUint16(at + 4, 20, true); // version needed
    view.setUint16(at + 6, 0x0800, true); // UTF-8 names
    view.setUint32(at + 14, entry.crc, true);
    view.setUint32(at + 18, entry.bytes.length, true);
    view.setUint32(at + 22, entry.bytes.length, true);
    view.setUint16(at + 26, entry.name.length, true);
    out.set(entry.name, at + 30);
    out.set(entry.bytes, at + 30 + entry.name.length);
    at += 30 + entry.name.length + entry.bytes.length;
  }
  const centralAt = at;
  entries.forEach((entry, index) => {
    view.setUint32(at, 0x02014b50, true);
    view.setUint16(at + 4, 20, true); // version made by
    view.setUint16(at + 6, 20, true);
    view.setUint16(at + 8, 0x0800, true);
    view.setUint32(at + 16, entry.crc, true);
    view.setUint32(at + 20, entry.bytes.length, true);
    view.setUint32(at + 24, entry.bytes.length, true);
    view.setUint16(at + 28, entry.name.length, true);
    view.setUint32(at + 42, offsets[index], true);
    out.set(entry.name, at + 46);
    at += 46 + entry.name.length;
  });
  view.setUint32(at, 0x06054b50, true);
  view.setUint16(at + 8, entries.length, true);
  view.setUint16(at + 10, entries.length, true);
  view.setUint32(at + 12, at - centralAt, true);
  view.setUint32(at + 16, centralAt, true);
  return out;
}
