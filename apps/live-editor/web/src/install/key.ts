/**
 * The key Live Editor signs packages with: made once in this browser and kept in IndexedDB. A
 * device that installed an app from a signed package takes updates to it only from the same key.
 */

import * as db from "../state/db";
import { newSigningKey, signerName } from "../package";

let loaded: Promise<CryptoKeyPair> | undefined;

/** This browser's signing key, made on first use. */
export function signingKey(): Promise<CryptoKeyPair> {
  loaded ??= (async () => {
    const kept = await db.get<CryptoKeyPair>("keys", "signing");
    if (kept) return kept;
    const key = await newSigningKey();
    await db.put("keys", "signing", key);
    return key;
  })();
  return loaded;
}

/** The signer's name as the device shows it, in two groups: `3f9a 21c0`. */
export async function shortSigner(key: CryptoKeyPair): Promise<string> {
  const name = await signerName(key.publicKey);
  return `${name.slice(0, 4)} ${name.slice(4, 8)}`;
}
