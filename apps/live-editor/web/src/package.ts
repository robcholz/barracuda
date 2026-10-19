/**
 * App packages, built in the browser: a container's files in one `.bapp`, signed or not, laid out
 * as docs/plugins/app-manager/app-format.mdx defines and as `pack-app.ts` packs them, so a package
 * from Live Editor is the one the toolchain would make.
 */

/** One file of a container: its name in the package and its bytes. */
export interface ContainerFile {
  name: string;
  bytes: Uint8Array;
}

/** The most files a package holds. */
export const MAX_FILES = 64;
/** The longest file name in a package, in UTF-8 bytes. */
export const MAX_NAME_BYTES = 32;
/** The largest package a device takes from the portal (`MAX_PACKAGE_BYTES` in the app manager). */
export const MAX_PACKAGE_BYTES = 1024 * 1024;

const ECDSA = { name: "ECDSA", namedCurve: "P-256" } as const;
const HEADER_BYTES = 12;
const ENTRY_BYTES = 40;
const SIGNED = 1;

/** Why a container cannot be packed. */
export class PackageError extends Error {}

const utf8 = (text: string) => new TextEncoder().encode(text);

/**
 * The files a package holds, in package order: `app.json`, the entry program, then the view's
 * program, styles and fonts as the manifest names them.
 */
export function packageFiles(
  files: ReadonlyMap<string, Uint8Array>,
): ContainerFile[] {
  const manifestBytes = files.get("app.json");
  if (!manifestBytes) throw new PackageError("the container has no app.json");
  const manifest = JSON.parse(new TextDecoder().decode(manifestBytes)) as {
    entry?: string;
    view?: { program: string; styles: string; fonts?: string[] };
  };
  const names = ["app.json", manifest.entry ?? "main.lua"];
  if (manifest.view)
    names.push(
      manifest.view.program,
      manifest.view.styles,
      ...(manifest.view.fonts ?? []),
    );
  return names.map((name) => {
    const bytes = files.get(name);
    if (!bytes) throw new PackageError(`the container has no ${name}`);
    return { name, bytes };
  });
}

/** Packs `files` into a `.bapp`, signed with `key` when one is given. */
export async function pack(
  files: readonly ContainerFile[],
  key?: CryptoKeyPair,
): Promise<Uint8Array> {
  if (files.length > MAX_FILES)
    throw new PackageError(`a package holds at most ${MAX_FILES} files`);
  for (const file of files)
    if (utf8(file.name).length > MAX_NAME_BYTES)
      throw new PackageError(
        `${file.name}: a file name in a package is at most ${MAX_NAME_BYTES} bytes`,
      );
  // The header, the directory, then the files.
  const directoryEnd = HEADER_BYTES + files.length * ENTRY_BYTES;
  const contentLength =
    directoryEnd + files.reduce((sum, file) => sum + file.bytes.length, 0);
  const content = new Uint8Array(contentLength);
  const view = new DataView(content.buffer);
  content.set(utf8("BAPP"), 0);
  content[4] = 1;
  content[5] = files.length;
  content[6] = key ? SIGNED : 0;
  view.setUint32(8, contentLength, true);
  let offset = directoryEnd;
  files.forEach((file, index) => {
    const entry = HEADER_BYTES + index * ENTRY_BYTES;
    content.set(utf8(file.name), entry);
    view.setUint32(entry + 32, offset, true);
    view.setUint32(entry + 36, file.bytes.length, true);
    content.set(file.bytes, offset);
    offset += file.bytes.length;
  });
  let packed = content;
  if (key) {
    const raw = await rawPublicKey(key.publicKey);
    // WebCrypto signs as `r || s`, the form devices read.
    const signature = new Uint8Array(
      await crypto.subtle.sign(
        { name: "ECDSA", hash: "SHA-256" },
        key.privateKey,
        content,
      ),
    );
    packed = new Uint8Array(contentLength + 4 + raw.length + signature.length);
    packed.set(content, 0);
    packed.set(utf8("BSIG"), contentLength);
    packed.set(raw, contentLength + 4);
    packed.set(signature, contentLength + 4 + raw.length);
  }
  if (packed.length > MAX_PACKAGE_BYTES)
    throw new PackageError(
      `the package is ${packed.length} bytes; a device takes at most ${MAX_PACKAGE_BYTES}`,
    );
  return packed;
}

/**
 * A new P-256 signing key. Keep it and sign every version of an app with it: a device that
 * installed the app from a signed package takes updates only from the same key.
 */
export function newSigningKey(): Promise<CryptoKeyPair> {
  return crypto.subtle.generateKey(ECDSA, true, ["sign", "verify"]);
}

/** The SEC1 uncompressed public key, as a signed package carries it. */
async function rawPublicKey(key: CryptoKey): Promise<Uint8Array<ArrayBuffer>> {
  return new Uint8Array(await crypto.subtle.exportKey("raw", key));
}

/** The name a device shows for a signer: the first 8 bytes of its key's SHA-256, in hex. */
export async function signerName(key: CryptoKey): Promise<string> {
  const digest = new Uint8Array(
    await crypto.subtle.digest("SHA-256", await rawPublicKey(key)),
  );
  return [...digest.slice(0, 8)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
