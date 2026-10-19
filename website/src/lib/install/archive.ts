/** Reads the `.tar.gz` release archive a visitor drops on the installer. */

const BLOCK = 512;

/** Inflates gzip data with the browser's own decompressor. */
export async function gunzip(bytes: Uint8Array): Promise<Uint8Array> {
  const stream = new Blob([bytes as BlobPart])
    .stream()
    .pipeThrough(new DecompressionStream("gzip"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

function text(bytes: Uint8Array): string {
  const end = bytes.indexOf(0);
  return new TextDecoder().decode(end < 0 ? bytes : bytes.subarray(0, end));
}

function octal(bytes: Uint8Array): number {
  const value = text(bytes).trim();
  if (!/^[0-7]*$/.test(value)) throw new Error("not a tar archive");
  return value === "" ? 0 : parseInt(value, 8);
}

/** The `path` record of a pax extended header, if it has one. */
function paxPath(data: Uint8Array): string | undefined {
  for (const record of new TextDecoder().decode(data).split("\n")) {
    const match = /^\d+ path=(.*)$/.exec(record);
    if (match) return match[1];
  }
  return undefined;
}

/**
 * The regular files of a tar archive by path. Directories and links are
 * skipped; GNU long names and pax paths are honored.
 */
export function untar(bytes: Uint8Array): Map<string, Uint8Array> {
  const files = new Map<string, Uint8Array>();
  let longName: string | undefined;
  let offset = 0;
  while (offset + BLOCK <= bytes.length) {
    const header = bytes.subarray(offset, offset + BLOCK);
    if (header.every((byte) => byte === 0)) break;
    const size = octal(header.subarray(124, 136));
    const type = String.fromCharCode(header[156]);
    const data = bytes.subarray(offset + BLOCK, offset + BLOCK + size);
    if (data.length !== size) throw new Error("truncated tar archive");
    offset += BLOCK + Math.ceil(size / BLOCK) * BLOCK;
    if (type === "L") {
      longName = text(data);
      continue;
    }
    if (type === "x") {
      longName = paxPath(data) ?? longName;
      continue;
    }
    if (type === "g") continue;
    const ustar = text(header.subarray(257, 262)) === "ustar";
    const prefix = ustar ? text(header.subarray(345, 500)) : "";
    const name =
      longName ??
      (prefix ? `${prefix}/` : "") + text(header.subarray(0, 100));
    longName = undefined;
    if (type === "0" || type === "\0") files.set(name, data);
  }
  return files;
}
