import { expect, test } from "bun:test";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { untar } from "../../website/src/lib/install/archive";
import {
  hotspotName,
  joinNetworkPayload,
  LineReader,
  parseConsoleLine,
  portalUrl,
} from "../../website/src/lib/install/console";
import {
  barracudaVersion,
  firstApp,
  parsePartitions,
  sameLayout,
} from "../../website/src/lib/install/flash-layout";
import {
  archiveName,
  openPackage,
  PackageError,
  type PackageProblem,
} from "../../website/src/lib/install/package";
import {
  archiveUrl,
  offeredReleases,
  parseReleases,
  releaseNotes,
} from "../../website/src/lib/install/releases";

const encoder = new TextEncoder();

function sha256(data: Uint8Array): string {
  return new Bun.CryptoHasher("sha256").update(data).digest("hex");
}

/** A package folder as `cargo image package` writes it, archived by GNU tar. */
async function releaseArchive(
  manifest: Record<string, unknown> = {},
  files: Record<string, Uint8Array> = {},
): Promise<Uint8Array> {
  const firmware = new Uint8Array([0xe9, 1, 2, 3, 4, 5]);
  const resources = new Uint8Array(1024).fill(7);
  const root = await mkdtemp(join(tmpdir(), "barracuda-install-"));
  const folder = join(root, "barracuda-0.1.0-esp32c3-devkitc-02");
  await mkdir(folder);
  const body = {
    version: "0.1.0",
    board: "esp32c3-devkitc-02",
    chip: "esp32c3",
    platform: "esp32c3",
    parts: [
      {
        path: "firmware.bin",
        format: "raw",
        offset: 0,
        size: firmware.length,
        sha256: sha256(firmware),
      },
      {
        path: "resources.img",
        format: "raw",
        offset: 0xaa0000,
        size: resources.length,
        sha256: sha256(resources),
      },
    ],
    ...manifest,
  };
  const contents: Record<string, Uint8Array> = {
    "firmware.bin": firmware,
    "resources.img": resources,
    "manifest.json": encoder.encode(JSON.stringify(body)),
    ...files,
  };
  for (const [name, data] of Object.entries(contents))
    await writeFile(join(folder, name), data);
  const tar = Bun.spawnSync([
    "tar",
    "-C",
    root,
    "-czf",
    "-",
    "barracuda-0.1.0-esp32c3-devkitc-02",
  ]);
  expect(tar.exitCode).toBe(0);
  return new Uint8Array(tar.stdout);
}

const expected = { board: "esp32c3-devkitc-02", version: "0.1.0" };

async function problem(archive: Uint8Array): Promise<PackageProblem> {
  try {
    await openPackage(archive, expected);
  } catch (error) {
    if (error instanceof PackageError) return error.problem;
    throw error;
  }
  throw new Error("the package opened");
}

test("a release archive opens into its raw parts at their offsets", async () => {
  const opened = await openPackage(await releaseArchive(), expected);
  expect(opened.manifest.board).toBe("esp32c3-devkitc-02");
  expect(opened.parts.map((part) => [part.path, part.offset])).toEqual([
    ["firmware.bin", 0],
    ["resources.img", 0xaa0000],
  ]);
  expect(opened.parts[0].data[0]).toBe(0xe9);
  expect(opened.bytes).toBe(6 + 1024);
});

test("another Board's, another version's or a damaged archive is refused", async () => {
  expect(
    await problem(await releaseArchive({ board: "esp32s3-box-3" })),
  ).toEqual({ kind: "other-board", board: "esp32s3-box-3" });
  expect(await problem(await releaseArchive({ version: "0.2.0" }))).toEqual({
    kind: "other-version",
    version: "0.2.0",
  });
  expect(
    await problem(
      await releaseArchive({}, { "resources.img": new Uint8Array(1024) }),
    ),
  ).toEqual({ kind: "corrupt", path: "resources.img" });
  expect(await problem(new Uint8Array([1, 2, 3]))).toEqual({
    kind: "unreadable",
  });
  expect(
    await problem(
      await releaseArchive({
        parts: [{ path: "firmware.elf", format: "elf", size: 1, sha256: "" }],
      }),
    ),
  ).toEqual({ kind: "not-flashable" });
});

test("an archive without a manifest is not a Barracuda release", async () => {
  const gzip = Bun.gzipSync(new Uint8Array(1024));
  expect(await problem(gzip)).toEqual({ kind: "not-barracuda" });
});

test("tar long names are honored", () => {
  const block = (fill: (header: Uint8Array) => void) => {
    const header = new Uint8Array(512);
    fill(header);
    return header;
  };
  const put = (header: Uint8Array, at: number, text: string) =>
    header.set(encoder.encode(text), at);
  const name = `${"d".repeat(120)}/manifest.json`;
  const longName = encoder.encode(name);
  const archive = new Uint8Array([
    ...block((h) => {
      put(h, 0, "././@LongLink");
      put(h, 124, longName.length.toString(8).padStart(11, "0"));
      put(h, 156, "L");
    }),
    ...longName,
    ...new Uint8Array(512 - longName.length),
    ...block((h) => {
      put(h, 0, "short");
      put(h, 124, "00000000002");
      put(h, 156, "0");
    }),
    ...encoder.encode("{}"),
    ...new Uint8Array(510),
    ...new Uint8Array(1024),
  ]);
  expect([...untar(archive).keys()]).toEqual([name]);
});

test("console lines become installer events", () => {
  expect(
    parseConsoleLine(
      "INFO - BARRACUDA system=boot board=esp32c3-devkitc-02 version=0.1.0\r",
    ),
  ).toEqual({ kind: "boot", board: "esp32c3-devkitc-02", version: "0.1.0" });
  expect(
    parseConsoleLine(
      "\x1b[32mINFO\x1b[0m BARRACUDA wifi=hotspot ip=192.168.4.1 ssid=Barracuda-B2C3",
    ),
  ).toEqual({ kind: "hotspot", ip: "192.168.4.1", ssid: "Barracuda-B2C3" });
  expect(
    parseConsoleLine("BARRACUDA wifi=connecting ssid=My Home Wi-Fi"),
  ).toEqual({
    kind: "connecting",
    ssid: "My Home Wi-Fi",
  });
  expect(
    parseConsoleLine(
      "BARRACUDA wifi=failed reason=connection_failed ssid=a b=c",
    ),
  ).toEqual({ kind: "failed", reason: "connection_failed", ssid: "a b=c" });
  expect(
    parseConsoleLine("BARRACUDA wifi=online ip=192.168.1.42 port=8787"),
  ).toEqual({ kind: "online", ip: "192.168.1.42", port: 8787 });
  expect(parseConsoleLine("INFO - starting plugins")).toBeNull();
});

test("console bytes split into lines across chunks", () => {
  const reader = new LineReader();
  expect(reader.push(encoder.encode("BARRACUDA wifi=on"))).toEqual([]);
  expect(reader.push(encoder.encode("line ip=1\nnext\n"))).toEqual([
    "BARRACUDA wifi=online ip=1",
    "next",
  ]);
});

test("the hotspot, its QR payload and the portal address follow the firmware", () => {
  expect(hotspotName("3c:84:27:a1:b2:c3")).toBe("Barracuda-B2C3");
  expect(hotspotName("unknown")).toBe("Barracuda Setup");
  expect(joinNetworkPayload("Barracuda-B2C3")).toBe(
    "WIFI:T:nopass;S:Barracuda-B2C3;;",
  );
  expect(joinNetworkPayload('a;b,c:d"e\\')).toBe(
    'WIFI:T:nopass;S:a\\;b\\,c\\:d\\"e\\\\;;',
  );
  expect(portalUrl("192.168.1.42", 8787)).toBe(
    "http://192.168.1.42:8787/portal/",
  );
});

function partitionTable(
  entries: [string, number, number, number, number][],
): Uint8Array {
  const table = new Uint8Array(0xc00).fill(0xff);
  const view = new DataView(table.buffer);
  entries.forEach(([label, type, subtype, offset, size], index) => {
    const at = index * 32;
    table.set([0xaa, 0x50, type, subtype], at);
    view.setUint32(at + 4, offset, true);
    view.setUint32(at + 8, size, true);
    table.fill(0, at + 12, at + 32);
    table.set(encoder.encode(label), at + 12);
  });
  return table;
}

const RELEASE_TABLE = partitionTable([
  ["nvs", 1, 2, 0x9000, 0x6000],
  ["ota_0", 0, 0x10, 0x20000, 0x400000],
  ["kv_database", 1, 0x80, 0x820000, 0x40000],
]);

test("a device with the release's partition table keeps its settings", () => {
  expect(firstApp(parsePartitions(RELEASE_TABLE))?.offset).toBe(0x20000);
  expect(sameLayout(RELEASE_TABLE, RELEASE_TABLE)).toBe(true);
  expect(
    sameLayout(
      partitionTable([
        ["nvs", 1, 2, 0x9000, 0x6000],
        ["factory", 0, 0, 0x10000, 0x100000],
      ]),
      RELEASE_TABLE,
    ),
  ).toBe(false);
  expect(sameLayout(new Uint8Array(0xc00).fill(0xff), RELEASE_TABLE)).toBe(
    false,
  );
});

test("the app descriptor names the installed Barracuda", () => {
  const descriptor = new Uint8Array(0x50);
  new DataView(descriptor.buffer).setUint32(0, 0xabcd5432, true);
  descriptor.set(encoder.encode("0.1.0"), 16);
  descriptor.set(encoder.encode("barracuda"), 48);
  expect(barracudaVersion(descriptor)).toBe("0.1.0");
  descriptor.set(encoder.encode("other\0\0\0\0"), 48);
  expect(barracudaVersion(descriptor)).toBeNull();
  expect(barracudaVersion(new Uint8Array(0x50))).toBeNull();
});

const GITHUB_RELEASES = [
  {
    tag_name: "v0.2.0-rc.1",
    draft: false,
    prerelease: true,
    html_url: "https://github.com/robcholz/barracuda/releases/tag/v0.2.0-rc.1",
    body: "",
    assets: [],
  },
  {
    tag_name: "v0.1.0",
    draft: false,
    prerelease: false,
    html_url: "https://github.com/robcholz/barracuda/releases/tag/v0.1.0",
    body: "Firmware for every device Board.\n\n## What's Changed\n* Name the setup hotspot per board by @someone in https://github.com/robcholz/barracuda/pull/110\n- **Web chat** can stop a reply, see [docs](https://x)\n",
    assets: [
      {
        name: archiveName("0.1.0", "esp32c3-devkitc-02"),
        browser_download_url:
          "https://github.com/robcholz/barracuda/releases/download/v0.1.0/barracuda-0.1.0-esp32c3-devkitc-02.tar.gz",
      },
    ],
  },
  { tag_name: "v0.0.9", draft: true, prerelease: false, html_url: "" },
];

test("releases list the stable version first, then a newer prerelease", () => {
  const releases = parseReleases(GITHUB_RELEASES);
  expect(releases.map((release) => release.tag)).toEqual([
    "v0.2.0-rc.1",
    "v0.1.0",
  ]);
  expect(offeredReleases(releases).map((release) => release.version)).toEqual([
    "0.1.0",
    "0.2.0-rc.1",
  ]);
  expect(offeredReleases(releases.slice(1)).length).toBe(1);
  expect(archiveUrl(releases[1], "esp32c3-devkitc-02")).toBe(
    "https://github.com/robcholz/barracuda/releases/download/v0.1.0/barracuda-0.1.0-esp32c3-devkitc-02.tar.gz",
  );
  expect(archiveUrl(releases[1], "esp32s3-box-3")).toBeNull();
  expect(releases[1].notes).toEqual([
    "Name the setup hotspot per board",
    "Web chat can stop a reply, see docs",
  ]);
  expect(releaseNotes("no bullets")).toEqual([]);
  expect(parseReleases({ message: "rate limited" })).toEqual([]);
});
