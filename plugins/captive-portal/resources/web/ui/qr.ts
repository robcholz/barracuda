/**
 * A QR Code encoder written for the portal from ISO/IEC 18004:2015: byte mode (UTF-8), error
 * correction level M, versions 1 to 40, the smallest version that fits. The data mask is chosen by
 * the standard's penalty rules (7.8.3), evaluated before the format and version information are
 * drawn, so the symbols match segno's for the same input.
 */

/** Error correction codewords per block, level M, versions 1–40 (Table 9). */
const ECC_PER_BLOCK = [
  10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26,
  26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
  28, 28,
];
/** Error correction blocks, level M, versions 1–40 (Table 9). */
const BLOCKS = [
  1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18,
  20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
];

/** A QR Code symbol: `modules[y * size + x]` is 1 for a dark module. */
export interface QrCode {
  version: number;
  mask: number;
  size: number;
  modules: Uint8Array;
}

// GF(256) with the QR polynomial x^8 + x^4 + x^3 + x^2 + 1
const EXP = new Uint8Array(510);
const LOG = new Uint8Array(256);
for (let i = 0, x = 1; i < 255; i++) {
  EXP[i] = EXP[i + 255] = x;
  LOG[x] = i;
  x = x & 0x80 ? (x << 1) ^ 0x11d : x << 1;
}
const mul = (a: number, b: number) => (a && b ? EXP[LOG[a] + LOG[b]] : 0);

/** Modules left for codewords once the function patterns are drawn. */
function rawModules(version: number) {
  let modules = (16 * version + 128) * version + 64;
  if (version >= 2) {
    const align = Math.floor(version / 7) + 2;
    modules -= (25 * align - 10) * align - 55;
    if (version >= 7) modules -= 36;
  }
  return modules;
}

const dataCodewords = (version: number) =>
  (rawModules(version) >> 3) - ECC_PER_BLOCK[version - 1] * BLOCKS[version - 1];

/** Row/column centres of the alignment patterns (Annex E). */
function alignment(version: number): number[] {
  if (version === 1) return [];
  const count = Math.floor(version / 7) + 2;
  const step =
    version === 32 ? 26 : Math.ceil((version * 4 + 4) / (count * 2 - 2)) * 2;
  const out = [6];
  for (let at = version * 4 + 10; out.length < count; at -= step)
    out.splice(1, 0, at);
  return out;
}

/** Reed–Solomon error correction codewords for one block. */
function correction(data: Uint8Array, degree: number): Uint8Array {
  const divisor = new Uint8Array(degree);
  divisor[degree - 1] = 1;
  for (let i = 0, root = 1; i < degree; i++, root = mul(root, 2))
    for (let j = 0; j < degree; j++) {
      divisor[j] = mul(divisor[j], root);
      if (j + 1 < degree) divisor[j] ^= divisor[j + 1];
    }
  const rest = new Uint8Array(degree);
  for (const byte of data) {
    const factor = byte ^ rest[0];
    rest.copyWithin(0, 1);
    rest[degree - 1] = 0;
    for (let i = 0; i < degree; i++) rest[i] ^= mul(divisor[i], factor);
  }
  return rest;
}

/** The data codewords followed by their error correction, interleaved by block (7.6). */
function codewords(bytes: Uint8Array, version: number): Uint8Array {
  const capacity = dataCodewords(version) * 8;
  const bits: number[] = [];
  const put = (value: number, length: number) => {
    for (let i = length - 1; i >= 0; i--) bits.push((value >>> i) & 1);
  };
  put(0b0100, 4);
  put(bytes.length, version < 10 ? 8 : 16);
  for (const byte of bytes) put(byte, 8);
  put(0, Math.min(4, capacity - bits.length));
  put(0, (8 - (bits.length % 8)) % 8);
  for (let pad = 0xec; bits.length < capacity; pad ^= 0xec ^ 0x11) put(pad, 8);
  const data = new Uint8Array(capacity / 8);
  bits.forEach((bit, i) => (data[i >> 3] |= bit << (7 - (i & 7))));

  const count = BLOCKS[version - 1];
  const degree = ECC_PER_BLOCK[version - 1];
  const raw = rawModules(version) >> 3;
  const short = count - (raw % count);
  const shortLength = Math.floor(raw / count) - degree;
  const blocks: Uint8Array[] = [];
  const checks: Uint8Array[] = [];
  for (let i = 0, at = 0; i < count; i++) {
    const block = data.subarray(at, (at += shortLength + (i < short ? 0 : 1)));
    blocks.push(block);
    checks.push(correction(block, degree));
  }
  const out: number[] = [];
  for (let i = 0; i <= shortLength; i++)
    for (const block of blocks) if (i < block.length) out.push(block[i]);
  for (let i = 0; i < degree; i++)
    for (const check of checks) out.push(check[i]);
  return Uint8Array.from(out);
}

const MASKS: ((x: number, y: number) => boolean)[] = [
  (x, y) => (x + y) % 2 === 0,
  (_, y) => y % 2 === 0,
  (x) => x % 3 === 0,
  (x, y) => (x + y) % 3 === 0,
  (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0,
  (x, y) => ((x * y) % 2) + ((x * y) % 3) === 0,
  (x, y) => (((x * y) % 2) + ((x * y) % 3)) % 2 === 0,
  (x, y) => (((x + y) % 2) + ((x * y) % 3)) % 2 === 0,
];

/** Occurrences of 1:1:3:1:1 with four light modules on either side, 40 points each. */
function finderLike(line: Uint8Array): number {
  const n = line.length;
  let score = 0;
  for (let at = 0; at + 7 <= n;) {
    if (
      line[at] &&
      !line[at + 1] &&
      line[at + 2] &&
      line[at + 3] &&
      line[at + 4] &&
      !line[at + 5] &&
      line[at + 6]
    ) {
      const before = line.subarray(Math.max(at - 4, 0), at).every((m) => !m);
      const after = line.subarray(at + 7, at + 11).every((m) => !m);
      if (at === 0 || at === n - 7 || before || after) {
        score += 40;
        at += 7;
      } else at += 4;
    } else at++;
  }
  return score;
}

/** The penalty score of a masked symbol (7.8.3.1, Table 11). */
function penalty(modules: Uint8Array, size: number): number {
  let score = 0;
  let dark = 0;
  const row = new Uint8Array(size);
  const column = new Uint8Array(size);
  for (let i = 0; i < size; i++) {
    let rowRun = 0;
    let columnRun = 0;
    for (let j = 0; j < size; j++) {
      row[j] = modules[i * size + j];
      column[j] = modules[j * size + i];
      dark += row[j];
      if (j && row[j] === row[j - 1]) rowRun++;
      else {
        if (rowRun >= 5) score += rowRun - 2;
        rowRun = 1;
      }
      if (j && column[j] === column[j - 1]) columnRun++;
      else {
        if (columnRun >= 5) score += columnRun - 2;
        columnRun = 1;
      }
      if (
        i &&
        j &&
        row[j] === row[j - 1] &&
        row[j] === modules[(i - 1) * size + j] &&
        row[j] === modules[(i - 1) * size + j - 1]
      )
        score += 3;
    }
    if (rowRun >= 5) score += rowRun - 2;
    if (columnRun >= 5) score += columnRun - 2;
    score += finderLike(row) + finderLike(column);
  }
  return (
    score + 10 * Math.floor(Math.abs((dark / (size * size)) * 100 - 50) / 5)
  );
}

/**
 * Encodes `text` (UTF-8, byte mode, level M) in the smallest version that holds it. `mask` forces a
 * data mask (0–7) instead of the one with the lowest penalty. Throws a `RangeError` past version 40.
 */
export function encodeQr(text: string, mask?: number): QrCode {
  const bytes = new TextEncoder().encode(text);
  let version = 1;
  while (
    4 + (version < 10 ? 8 : 16) + bytes.length * 8 >
    dataCodewords(version) * 8
  )
    if (++version > 40) throw new RangeError("Too much data for a QR Code");

  const size = version * 4 + 17;
  const modules = new Uint8Array(size * size);
  const reserved = new Uint8Array(size * size);
  const set = (x: number, y: number, dark: boolean | number) => {
    modules[y * size + x] = dark ? 1 : 0;
    reserved[y * size + x] = 1;
  };

  for (let i = 0; i < size; i++) {
    set(6, i, i % 2 === 0);
    set(i, 6, i % 2 === 0);
  }
  for (const [cx, cy] of [
    [3, 3],
    [size - 4, 3],
    [3, size - 4],
  ])
    for (let dy = -4; dy <= 4; dy++)
      for (let dx = -4; dx <= 4; dx++) {
        const x = cx + dx;
        const y = cy + dy;
        const ring = Math.max(Math.abs(dx), Math.abs(dy));
        if (x >= 0 && x < size && y >= 0 && y < size)
          set(x, y, ring !== 2 && ring !== 4);
      }
  const centres = alignment(version);
  const last = centres.length - 1;
  centres.forEach((cy, i) =>
    centres.forEach((cx, j) => {
      if (
        (i === 0 && j === 0) ||
        (i === 0 && j === last) ||
        (i === last && j === 0)
      )
        return;
      for (let dy = -2; dy <= 2; dy++)
        for (let dx = -2; dx <= 2; dx++)
          set(cx + dx, cy + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
    }),
  );

  // format (15 bits) and version (18 bits) information, all light while the mask is chosen
  const format = (pattern: number, on: boolean) => {
    let rest = pattern; // level M is 00
    for (let i = 0; i < 10; i++) rest = (rest << 1) ^ ((rest >>> 9) * 0x537);
    const bits = ((pattern << 10) | rest) ^ 0x5412;
    const bit = (i: number) => on && (bits >>> i) & 1;
    for (let i = 0; i < 6; i++) set(8, i, bit(i));
    set(8, 7, bit(6));
    set(8, 8, bit(7));
    set(7, 8, bit(8));
    for (let i = 9; i < 15; i++) set(14 - i, 8, bit(i));
    for (let i = 0; i < 8; i++) set(size - 1 - i, 8, bit(i));
    for (let i = 8; i < 15; i++) set(8, size - 15 + i, bit(i));
    set(8, size - 8, on); // the dark module
  };
  const versionInfo = (on: boolean) => {
    if (version < 7) return;
    let rest = version;
    for (let i = 0; i < 12; i++) rest = (rest << 1) ^ ((rest >>> 11) * 0x1f25);
    const bits = (version << 12) | rest;
    for (let i = 0; i < 18; i++) {
      const dark = on && (bits >>> i) & 1;
      const a = size - 11 + (i % 3);
      const b = Math.floor(i / 3);
      set(a, b, dark);
      set(b, a, dark);
    }
  };
  format(0, false);
  versionInfo(false);

  const data = codewords(bytes, version);
  for (let i = 0, right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let vertical = 0; vertical < size; vertical++)
      for (let j = 0; j < 2; j++) {
        const x = right - j;
        const y = (right + 1) & 2 ? vertical : size - 1 - vertical;
        if (!reserved[y * size + x] && i < data.length * 8) {
          modules[y * size + x] = (data[i >>> 3] >>> (7 - (i & 7))) & 1;
          i++;
        }
      }
  }

  const masked = (pattern: number) => {
    const out = modules.slice();
    for (let y = 0; y < size; y++)
      for (let x = 0; x < size; x++)
        if (!reserved[y * size + x] && MASKS[pattern](x, y))
          out[y * size + x] ^= 1;
    return out;
  };
  let chosen = mask ?? 0;
  if (mask === undefined) {
    let best = Infinity;
    for (let pattern = 0; pattern < 8; pattern++) {
      const score = penalty(masked(pattern), size);
      if (score < best) [best, chosen] = [score, pattern];
    }
  }
  modules.set(masked(chosen));
  format(chosen, true);
  versionInfo(true);
  return { version, mask: chosen, size, modules };
}

/**
 * The dark modules of `code` as one SVG path in a `size + 2 × quiet` square, one rectangle per
 * horizontal run, as the design draws its codes.
 */
export function qrPath(code: QrCode, quiet = 2): string {
  const { size, modules } = code;
  let d = "";
  for (let y = 0; y < size; y++)
    for (let x = 0; x < size;) {
      if (!modules[y * size + x]) {
        x++;
        continue;
      }
      const start = x;
      while (x < size && modules[y * size + x]) x++;
      d += `M${start + quiet} ${y + quiet}h${x - start}v1h${start - x}z`;
    }
  return d;
}

const SVG = "http://www.w3.org/2000/svg";

/**
 * `text` as a QR Code `<svg>`, `px` square. Put it directly in a `.bc-qr` plate (`qrPlate` in
 * `./blocks`), which fills the dark modules `foreground` and stays light in the dark theme: scanners
 * need dark on light.
 */
export function qrSvg(text: string, px: number, quiet = 2): SVGSVGElement {
  const code = encodeQr(text);
  const box = code.size + quiet * 2;
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", `0 0 ${box} ${box}`);
  svg.setAttribute("width", String(px));
  svg.setAttribute("height", String(px));
  svg.setAttribute("shape-rendering", "crispEdges");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(SVG, "path");
  path.setAttribute("d", qrPath(code, quiet));
  svg.append(path);
  return svg;
}
