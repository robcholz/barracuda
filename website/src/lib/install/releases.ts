/**
 * Barracuda's GitHub Releases, as the installer lists them. The API allows
 * cross-site reads; the release files do not, so the visitor downloads the
 * archive and drops it back on the page.
 */

import { archiveName } from "./package";

export const REPOSITORY = "robcholz/barracuda";
const API = `https://api.github.com/repos/${REPOSITORY}/releases?per_page=20`;

/** One published release. */
export type Release = {
  tag: string;
  /** The tag without its leading `v`, as the firmware and archives name it. */
  version: string;
  prerelease: boolean;
  url: string;
  notes: string[];
  /** Release archive name → download URL. */
  assets: Map<string, string>;
};

type GitHubRelease = {
  tag_name: string;
  draft: boolean;
  prerelease: boolean;
  html_url: string;
  body?: string | null;
  assets?: { name: string; browser_download_url: string }[];
};

/** The first bullet points of a release body, without Markdown. */
export function releaseNotes(body: string, limit = 5): string[] {
  return body
    .split("\n")
    .map((line) => /^\s*[-*]\s+(.*)$/.exec(line)?.[1])
    .filter((line): line is string => !!line)
    .map((line) =>
      line
        // GitHub's generated notes end each change with its author and PR.
        .replace(/\s+by @\S+ in https?:\/\/\S+$/, "")
        .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
        .replace(/[*_`]/g, "")
        .trim(),
    )
    .filter(Boolean)
    .slice(0, limit);
}

/** Published releases, newest first, from the GitHub API's response. */
export function parseReleases(json: unknown): Release[] {
  if (!Array.isArray(json)) return [];
  return (json as GitHubRelease[])
    .filter((release) => !release.draft && release.tag_name)
    .map((release) => ({
      tag: release.tag_name,
      version: release.tag_name.replace(/^v/, ""),
      prerelease: release.prerelease,
      url: release.html_url,
      notes: releaseNotes(release.body ?? ""),
      assets: new Map(
        (release.assets ?? []).map((asset) => [
          asset.name,
          asset.browser_download_url,
        ]),
      ),
    }));
}

/** Where to download one Board's archive of a release, if it has one. */
export function archiveUrl(release: Release, board: string): string | null {
  return release.assets.get(archiveName(release.version, board)) ?? null;
}

/** The newest stable release first, then any newer prerelease. */
export function offeredReleases(releases: Release[]): Release[] {
  const stable = releases.find((release) => !release.prerelease);
  const preview = releases.find((release) => release.prerelease);
  const offered = stable ? [stable] : [];
  if (preview && (!stable || releases.indexOf(preview) < releases.indexOf(stable)))
    offered.push(preview);
  return offered;
}

/** Fetches the published releases. */
export async function fetchReleases(): Promise<Release[]> {
  const response = await fetch(API, {
    headers: { Accept: "application/vnd.github+json" },
  });
  if (!response.ok) throw new Error(`GitHub answered ${response.status}`);
  return parseReleases(await response.json());
}
