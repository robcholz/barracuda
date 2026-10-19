/**
 * The release package of one Board: `cargo image package` writes it, the
 * release workflow archives it as `barracuda-<version>-<board>.tar.gz`, and
 * the installer checks it before writing anything.
 */

import { gunzip, untar } from "./archive";

/** One file of a package, as `manifest.json` lists it. */
export type ManifestPart = {
  path: string;
  format: "raw" | "elf";
  /** The flash offset of a raw image; an ELF carries its own addresses. */
  offset?: number;
  size: number;
  sha256: string;
};

/** A package's `manifest.json`. */
export type Manifest = {
  version: string;
  board: string;
  chip: string;
  platform: string;
  parts: ManifestPart[];
};

/** A checked raw image and where it is written. */
export type FlashPart = { path: string; offset: number; data: Uint8Array };

/** A package whose every part matches its manifest. */
export type Package = { manifest: Manifest; parts: FlashPart[]; bytes: number };

/** Why a dropped file cannot be installed. */
export type PackageProblem =
  | { kind: "unreadable" }
  | { kind: "not-barracuda" }
  | { kind: "other-board"; board: string }
  | { kind: "other-version"; version: string }
  | { kind: "not-flashable" }
  | { kind: "corrupt"; path: string };

/** A dropped file the installer refuses, and why. */
export class PackageError extends Error {
  constructor(readonly problem: PackageProblem) {
    super(problem.kind);
  }
}

/** The release archive's file name for one Board. */
export function archiveName(version: string, board: string): string {
  return `barracuda-${version}-${board}.tar.gz`;
}

async function sha256(data: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", data as BufferSource);
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function isManifest(value: unknown): value is Manifest {
  const manifest = value as Manifest;
  return (
    typeof manifest?.version === "string" &&
    typeof manifest.board === "string" &&
    typeof manifest.chip === "string" &&
    Array.isArray(manifest.parts) &&
    manifest.parts.every(
      (part) =>
        typeof part?.path === "string" &&
        typeof part.size === "number" &&
        typeof part.sha256 === "string",
    )
  );
}

/**
 * Opens a dropped release archive and checks it is the expected Board and
 * version, with every part intact.
 *
 * @throws {PackageError} when the archive cannot be installed.
 */
export async function openPackage(
  archive: Uint8Array,
  expected: { board: string; version: string },
): Promise<Package> {
  let files: Map<string, Uint8Array>;
  try {
    files = untar(await gunzip(archive));
  } catch {
    throw new PackageError({ kind: "unreadable" });
  }
  // The archive holds one folder, `barracuda-<version>-<board>/`.
  const manifestPath = [...files.keys()].find(
    (path) => path.split("/").pop() === "manifest.json",
  );
  if (!manifestPath) throw new PackageError({ kind: "not-barracuda" });
  const folder = manifestPath.slice(0, -"manifest.json".length);
  let manifest: unknown;
  try {
    manifest = JSON.parse(
      new TextDecoder().decode(files.get(manifestPath)),
    );
  } catch {
    throw new PackageError({ kind: "not-barracuda" });
  }
  if (!isManifest(manifest)) throw new PackageError({ kind: "not-barracuda" });
  if (manifest.board !== expected.board)
    throw new PackageError({ kind: "other-board", board: manifest.board });
  if (manifest.version !== expected.version)
    throw new PackageError({
      kind: "other-version",
      version: manifest.version,
    });
  if (manifest.parts.some((part) => part.offset === undefined))
    throw new PackageError({ kind: "not-flashable" });

  const parts: FlashPart[] = [];
  for (const part of manifest.parts) {
    const data = files.get(folder + part.path);
    if (
      !data ||
      data.length !== part.size ||
      (await sha256(data)) !== part.sha256.toLowerCase()
    )
      throw new PackageError({ kind: "corrupt", path: part.path });
    parts.push({ path: part.path, offset: part.offset as number, data });
  }
  return {
    manifest,
    parts,
    bytes: parts.reduce((sum, part) => sum + part.data.length, 0),
  };
}
