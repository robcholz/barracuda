// Packs a built app container into one `.bapp` file, signed or not.
//
//   bun pack-app.ts <bundle> <out.bapp> [--key <key.jwk>]   pack, signing with the key
//   bun pack-app.ts --new-key <key.jwk>                     make a signing key
//
// A package holds the container's manifest and the files it names, laid out
// as `docs/app-format.md` defines. A signed one ends with the key's public
// half and its ECDSA P-256 signature over everything before; a device that
// installed an app from a signed package takes updates only from that key,
// so keep the key file safe and reuse it for every version. Offer the
// package in the Workspace as `/workspace/media/apps/<id>.bapp`.
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const ECDSA = { name: "ECDSA", namedCurve: "P-256" };
const args = process.argv.slice(2);
const option = (name: string) => {
  const at = args.indexOf(name);
  if (at < 0) return undefined;
  const [, value] = args.splice(at, 2);
  return value;
};

const newKey = option("--new-key");
if (newKey) {
  const pair = await crypto.subtle.generateKey(ECDSA, true, ["sign", "verify"]);
  writeFileSync(newKey, JSON.stringify(await crypto.subtle.exportKey("jwk", pair.privateKey), null, 2) + "\n", { mode: 0o600 });
  console.log(`${newKey}: a new P-256 signing key`);
  process.exit(0);
}
const keyFile = option("--key");
if (args.length !== 2) {
  console.error("Usage: bun pack-app.ts <bundle> <out.bapp> [--key <key.jwk>] | --new-key <key.jwk>");
  process.exit(1);
}
const [bundle, out] = args;

const manifestBytes = readFileSync(join(bundle, "app.json"));
const manifest = JSON.parse(manifestBytes.toString("utf8"));
const names: string[] = ["app.json", manifest.entry ?? "main.lua"];
if (manifest.view) names.push(manifest.view.program, manifest.view.styles, ...(manifest.view.fonts ?? []));
const files = names.map((name) => {
  if (new TextEncoder().encode(name).length > 32) throw new Error(`${name}: a file name in a package is at most 32 bytes`);
  return { name, bytes: name === "app.json" ? manifestBytes : readFileSync(join(bundle, name)) };
});
if (files.length > 64) throw new Error("a package holds at most 64 files");

// The header, the directory, then the files.
const directoryEnd = 12 + files.length * 40;
const contentLength = directoryEnd + files.reduce((sum, file) => sum + file.bytes.length, 0);
const content = new Uint8Array(contentLength);
const view = new DataView(content.buffer);
content.set(new TextEncoder().encode("BAPP"), 0);
content[4] = 1;
content[5] = files.length;
content[6] = keyFile ? 1 : 0;
view.setUint32(8, contentLength, true);
let offset = directoryEnd;
files.forEach((file, index) => {
  const entry = 12 + index * 40;
  content.set(new TextEncoder().encode(file.name), entry);
  view.setUint32(entry + 32, offset, true);
  view.setUint32(entry + 36, file.bytes.length, true);
  content.set(file.bytes, offset);
  offset += file.bytes.length;
});

let packed = content;
let signer = "unsigned";
if (keyFile) {
  const jwk = JSON.parse(readFileSync(keyFile, "utf8"));
  const privateKey = await crypto.subtle.importKey("jwk", jwk, ECDSA, false, ["sign"]);
  const publicKey = await crypto.subtle.importKey("jwk", { kty: jwk.kty, crv: jwk.crv, x: jwk.x, y: jwk.y }, ECDSA, true, ["verify"]);
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", publicKey));
  // WebCrypto signs as `r || s`, the form devices read.
  const signature = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, privateKey, content));
  packed = new Uint8Array(contentLength + 4 + raw.length + signature.length);
  packed.set(content, 0);
  packed.set(new TextEncoder().encode("BSIG"), contentLength);
  packed.set(raw, contentLength + 4);
  packed.set(signature, contentLength + 4 + raw.length);
  const fingerprint = new Uint8Array(await crypto.subtle.digest("SHA-256", raw)).slice(0, 8);
  signer = `signed by ${[...fingerprint].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}
writeFileSync(out, packed);
console.log(`${out}: ${manifest.id} ${manifest.version}, ${files.length} files, ${packed.length} bytes, ${signer}`);
