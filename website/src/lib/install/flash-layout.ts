/**
 * What the installer reads from an ESP Board's flash before writing: the
 * partition table at 0x8000 and the app descriptor of the `ota_0` image.
 */

/** Where the ESP-IDF partition table starts. */
export const PARTITION_TABLE_OFFSET = 0x8000;
/** The bytes the installer reads and compares, enough for any table here. */
export const PARTITION_TABLE_SIZE = 0xc00;
/** The app descriptor's offset in an app image, and the bytes it spans. */
export const APP_DESCRIPTOR_OFFSET = 0x20;
export const APP_DESCRIPTOR_SIZE = 0x50;

const ENTRY = 32;
const APP_DESCRIPTOR_MAGIC = 0xabcd5432;

/** One partition table entry. */
export type Partition = {
  label: string;
  type: number;
  subtype: number;
  offset: number;
  size: number;
};

function cString(bytes: Uint8Array): string {
  const end = bytes.indexOf(0);
  return new TextDecoder().decode(end < 0 ? bytes : bytes.subarray(0, end));
}

/** The entries of a binary partition table; empty when none is written. */
export function parsePartitions(table: Uint8Array): Partition[] {
  const view = new DataView(table.buffer, table.byteOffset, table.byteLength);
  const partitions: Partition[] = [];
  for (let at = 0; at + ENTRY <= table.length; at += ENTRY) {
    // 0xAA50 opens an entry; the MD5 entry (0xEBEB) or erased flash ends it.
    if (table[at] !== 0xaa || table[at + 1] !== 0x50) break;
    partitions.push({
      type: table[at + 2],
      subtype: table[at + 3],
      offset: view.getUint32(at + 4, true),
      size: view.getUint32(at + 8, true),
      label: cString(table.subarray(at + 12, at + 28)),
    });
  }
  return partitions;
}

/** Whether two tables declare the same partitions, so settings survive. */
export function sameLayout(device: Uint8Array, release: Uint8Array): boolean {
  const a = parsePartitions(device);
  const b = parsePartitions(release);
  return (
    a.length > 0 &&
    a.length === b.length &&
    a.every(
      (partition, index) =>
        JSON.stringify(partition) === JSON.stringify(b[index]),
    )
  );
}

/** The `ota_0` app partition of a table, where a release writes its app. */
export function firstApp(partitions: Partition[]): Partition | undefined {
  return partitions.find(
    (partition) => partition.type === 0 && partition.subtype === 0x10,
  );
}

/** The version a Barracuda app descriptor names, or `null` for other apps. */
export function barracudaVersion(descriptor: Uint8Array): string | null {
  if (descriptor.length < APP_DESCRIPTOR_SIZE) return null;
  const view = new DataView(
    descriptor.buffer,
    descriptor.byteOffset,
    descriptor.byteLength,
  );
  if (view.getUint32(0, true) !== APP_DESCRIPTOR_MAGIC) return null;
  if (cString(descriptor.subarray(48, 80)) !== "barracuda") return null;
  return cString(descriptor.subarray(16, 48)) || null;
}
