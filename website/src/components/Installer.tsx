"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BOARDS } from "@/content/boards";
import { installEn } from "@/content/install-en";
import { installZh } from "@/content/install-zh";
import type { Lang } from "@/lib/links";
import { type ConsoleEvent, hotspotName, parseConsoleLine } from "@/lib/install/console";
import { type Chip, type Installed, releaseTable, SerialBoard, serialSupported } from "@/lib/install/flasher";
import { parsePartitions } from "@/lib/install/flash-layout";
import { openPackage, type Package, PackageError, type PackageProblem } from "@/lib/install/package";
import { fetchReleases, offeredReleases, type Release } from "@/lib/install/releases";
import {
  BoardStep,
  CliStep,
  ConnectStep,
  DoneStep,
  FirmwareStep,
  FlashStep,
  Frame,
  WifiStep,
} from "./InstallerSteps";

/** A Board the installer cannot flash from the browser, and how to run it. */
export type CliKind = "stm32" | "linux" | "macos";

/** How a Board is installed: from the browser, or with these commands. */
export function cliKind(chip: string): CliKind | null {
  if (chip.startsWith("STM32")) return "stm32";
  if (chip === "Linux") return "linux";
  if (chip === "macOS") return "macos";
  return null;
}

/** esptool's chip name and the manifest's `esp32s3` name, compared. */
function sameChip(a: string, b: string): boolean {
  const plain = (name: string) => name.toLowerCase().replace(/[^a-z0-9]/g, "");
  return plain(a) === plain(b);
}

const SIZES: Record<string, number> = { KB: 1024, MB: 1024 * 1024 };

/** `16MB` in bytes; `null` when esptool could not tell. */
function flashBytes(size?: string): number | null {
  const match = /^(\d+)(KB|MB)$/.exec(size ?? "");
  return match ? Number(match[1]) * SIZES[match[2]] : null;
}

/** The flash a release's partition table needs, as esptool names sizes. */
function neededFlash(pkg: Package): { bytes: number; label: string } {
  const firmware = pkg.parts.find((part) => part.offset === 0);
  const partitions = firmware ? parsePartitions(releaseTable(firmware.data)) : [];
  const end = Math.max(0, ...partitions.map((part) => part.offset + part.size));
  let mebibytes = 1;
  while (mebibytes * 1024 * 1024 < end) mebibytes *= 2;
  return { bytes: end, label: `${mebibytes}MB` };
}

export type ThemePref = "light" | "dark" | "system";
export type FileState =
  | { state: "idle" }
  | { state: "checking"; name: string }
  | { state: "ok"; name: string; pkg: Package }
  | { state: "problem"; name: string; problem: PackageProblem };
export type DeviceState =
  | { state: "idle" | "connecting" | "failed" }
  | { state: "connected"; chip: Chip; installed: Installed }
  | { state: "mismatch"; chip: Chip }
  | { state: "small"; chip: Chip; needed: string };
export type FlashState = {
  state: "running" | "failed" | "done";
  pct: number;
  fresh: boolean;
  written: number;
};
export type WifiState = {
  ap: string;
  net: string | null;
  state: "waiting" | "joining" | "failed" | "online";
  ip: string | null;
  port: number;
  lost: boolean;
};

const THEME_KEY = "barracuda-install-theme";
const LOG_LIMIT = 4000;

/** The browser installer: Board, firmware, connect, write, Wi-Fi, done. */
export function Installer({ lang }: { lang: Lang }) {
  // The words hold functions, so they are picked here rather than passed in.
  const t = lang === "zh" ? installZh : installEn;
  const [step, setStep] = useState(1);
  const [themePref, setThemePref] = useState<ThemePref>("light");
  const [systemDark, setSystemDark] = useState(false);
  const [supported, setSupported] = useState(true);
  const [boardId, setBoardId] = useState<string | null>(null);
  const [chipFilter, setChipFilter] = useState("all");
  const [detected, setDetected] = useState<string | null>(null);
  const [detecting, setDetecting] = useState<"idle" | "busy" | "failed">("idle");
  const [releases, setReleases] = useState<
    { state: "loading" } | { state: "failed" } | { state: "ready"; list: Release[] }
  >({ state: "loading" });
  const [tag, setTag] = useState<string | null>(null);
  const [file, setFile] = useState<FileState>({ state: "idle" });
  const [device, setDevice] = useState<DeviceState>({ state: "idle" });
  const [fresh, setFresh] = useState(false);
  const [flash, setFlash] = useState<FlashState>({ state: "running", pct: 0, fresh: false, written: 0 });
  const [wifi, setWifi] = useState<WifiState>({
    ap: "Barracuda Setup",
    net: null,
    state: "waiting",
    ip: null,
    port: 8787,
    lost: false,
  });
  const [log, setLog] = useState<string[]>([]);
  const [toast, setToast] = useState<string | null>(null);
  const serial = useRef<SerialBoard | null>(null);
  const stopFollowing = useRef<(() => Promise<void>) | null>(null);

  // Browser-only facts: Web Serial, the remembered theme, the system theme.
  useEffect(() => {
    setSupported(serialSupported());
    try {
      const saved = localStorage.getItem(THEME_KEY);
      if (saved === "light" || saved === "dark" || saved === "system") setThemePref(saved);
    } catch {}
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    setSystemDark(media.matches);
    const change = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    media.addEventListener("change", change);
    return () => media.removeEventListener("change", change);
  }, []);

  const loadReleases = useCallback(() => {
    setReleases({ state: "loading" });
    fetchReleases()
      .then((list) => setReleases({ state: "ready", list }))
      .catch(() => setReleases({ state: "failed" }));
  }, []);
  useEffect(loadReleases, [loadReleases]);

  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), 4000);
    return () => clearTimeout(timer);
  }, [toast]);

  // The port is released when the visitor leaves the page.
  useEffect(
    () => () => {
      void stopFollowing.current?.();
      void serial.current?.close();
    },
    [],
  );

  const theme = themePref === "system" ? (systemDark ? "dark" : "light") : themePref;
  const chooseTheme = (pref: ThemePref) => {
    setThemePref(pref);
    try {
      localStorage.setItem(THEME_KEY, pref);
    } catch {}
  };

  const board = BOARDS.find((entry) => entry.id === boardId) ?? null;
  const cli = board ? cliKind(board.chip) : null;
  const offered = useMemo(
    () => (releases.state === "ready" ? offeredReleases(releases.list) : []),
    [releases],
  );
  const release = offered.find((entry) => entry.tag === tag) ?? offered[0] ?? null;
  const pkg = file.state === "ok" ? file.pkg : null;

  const append = useCallback((line: string) => {
    setLog((lines) => [...lines, line].slice(-LOG_LIMIT));
  }, []);

  /** The port the visitor picked, asking for one the first time. */
  const port = async (): Promise<SerialBoard | null> => {
    if (serial.current) return serial.current;
    try {
      serial.current = await SerialBoard.choose();
      return serial.current;
    } catch {
      // The visitor closed the browser's port dialog.
      return null;
    }
  };

  const go = (next: number) => setStep(next);

  // 01: reading the chip filters the list to its Boards.
  const detect = async () => {
    const chosen = await port();
    if (!chosen) return;
    setDetecting("busy");
    try {
      const chip = await chosen.connect(append);
      await chosen.close();
      setDetected(chip.name);
      setChipFilter(chip.name);
      setDetecting("idle");
    } catch (error) {
      append(String(error));
      await forget();
      setDetecting("failed");
    }
  };

  const pickBoard = (id: string) => {
    if (id === boardId) return;
    setBoardId(id);
    setFile({ state: "idle" });
    setDevice({ state: "idle" });
  };

  // 02: the visitor downloads the archive and drops it back.
  const pickRelease = (next: string) => {
    setTag(next);
    setFile({ state: "idle" });
  };
  const dropFile = async (dropped: File) => {
    if (!board || !release) return;
    setFile({ state: "checking", name: dropped.name });
    try {
      const opened = await openPackage(new Uint8Array(await dropped.arrayBuffer()), {
        board: board.id,
        version: release.version,
      });
      setFile({ state: "ok", name: dropped.name, pkg: opened });
    } catch (error) {
      setFile({
        state: "problem",
        name: dropped.name,
        problem: error instanceof PackageError ? error.problem : { kind: "unreadable" },
      });
    }
  };

  // 03: connect, check the chip and flash size, and read what is installed.
  const connect = async () => {
    if (!board || !pkg) return;
    const chosen = await port();
    if (!chosen) return;
    setDevice({ state: "connecting" });
    try {
      const chip = await chosen.connect(append);
      if (!sameChip(chip.name, board.chip) || !sameChip(chip.name, pkg.manifest.chip)) {
        await forget();
        setDevice({ state: "mismatch", chip });
        return;
      }
      const needed = neededFlash(pkg);
      const has = flashBytes(chip.flashSize);
      if (has !== null && has < needed.bytes) {
        await forget();
        setDevice({ state: "small", chip, needed: needed.label });
        return;
      }
      const firmware = pkg.parts.find((part) => part.offset === 0);
      const installed = firmware
        ? await chosen.inspect(releaseTable(firmware.data))
        : { version: null, sameLayout: false };
      setFresh(!installed.sameLayout);
      setWifi((current) => ({ ...current, ap: hotspotName(chip.mac) }));
      setDevice({ state: "connected", chip, installed });
    } catch (error) {
      append(String(error));
      await forget();
      setDevice({ state: "failed" });
    }
  };
  const disconnect = async () => {
    await serial.current?.close();
    setDevice({ state: "idle" });
  };
  /** Closes the port and asks for one again next time: it may be the wrong one. */
  async function forget() {
    await serial.current?.close();
    serial.current = null;
  }

  // 05: the console reports the hotspot, the join and the new address.
  const onEvent = useCallback((event: ConsoleEvent) => {
    setWifi((current) => {
      switch (event.kind) {
        case "hotspot":
          return { ...current, ap: event.ssid || current.ap };
        case "connecting":
          return { ...current, net: event.ssid, state: "joining" };
        case "failed":
          return { ...current, net: event.ssid || current.net, state: "failed" };
        case "online":
          return { ...current, state: "online", ip: event.ip, port: event.port };
        default:
          return current;
      }
    });
  }, []);
  const follow = useCallback(
    async (target: SerialBoard) => {
      await stopFollowing.current?.();
      setWifi((current) => ({ ...current, lost: false }));
      stopFollowing.current = await target.follow(
        (line) => {
          append(line);
          const event = parseConsoleLine(line);
          if (event) onEvent(event);
        },
        () => setWifi((current) => ({ ...current, lost: true })),
      );
    },
    [append, onEvent],
  );
  const reopen = async () => {
    try {
      const target = await SerialBoard.choose();
      serial.current = target;
      await follow(target);
    } catch {
      setWifi((current) => ({ ...current, lost: true }));
    }
  };

  // 04: write, restart, then follow the console straight away so no line is missed.
  const startFlash = async () => {
    const target = serial.current;
    if (!target || !pkg) return;
    const wipe = fresh;
    setStep(4);
    setFlash({ state: "running", pct: 4, fresh: wipe, written: 0 });
    let pct = 4;
    try {
      await target.write(pkg.parts, {
        erase: wipe,
        onErased: () => {
          pct = 14;
          setFlash((current) => ({ ...current, pct }));
        },
        onProgress: (_part, fraction) => {
          pct = 14 + Math.round(fraction * 80);
          setFlash((current) => ({ ...current, pct, written: Math.round(fraction * pkg.bytes) }));
        },
      });
      setFlash((current) => ({ ...current, pct: 94, written: pkg.bytes }));
      try {
        await follow(target);
      } catch {
        setWifi((current) => ({ ...current, lost: true }));
      }
      setFlash((current) => ({ ...current, state: "done", pct: 100 }));
    } catch (error) {
      append(String(error));
      await target.close();
      setFlash((current) => ({ ...current, state: "failed", pct }));
    }
  };
  const reconnect = async () => {
    await serial.current?.close();
    setDevice({ state: "idle" });
    setStep(3);
  };

  // 05 → 06 once the Board reports its address.
  useEffect(() => {
    if (step !== 5 || wifi.state !== "online") return;
    const timer = setTimeout(() => setStep(6), 1200);
    return () => clearTimeout(timer);
  }, [step, wifi.state]);

  const reset = async () => {
    await stopFollowing.current?.();
    stopFollowing.current = null;
    await serial.current?.close();
    serial.current = null;
    setStep(1);
    setBoardId(null);
    setFile({ state: "idle" });
    setDevice({ state: "idle" });
    setWifi({ ap: "Barracuda Setup", net: null, state: "waiting", ip: null, port: 8787, lost: false });
    setLog([]);
  };

  const downloadLog = () => {
    const url = URL.createObjectURL(new Blob([log.join("\n")], { type: "text/plain" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = t.log.file;
    link.click();
    URL.revokeObjectURL(url);
  };

  const copy = async (text: string, done: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setToast(done);
    } catch {}
  };

  const installedVersion =
    device.state === "connected" && device.installed.version ? device.installed.version : null;
  const deviceMac =
    device.state === "connected" ? device.chip.mac.split(":").slice(3).join(":").toUpperCase() : "—";
  const versionLabel = release ? release.tag : "—";

  const figureState =
    step === 3 && (device.state === "connected" || device.state === "mismatch" || device.state === "small")
      ? "connected"
      : step === 4
        ? flash.state === "running"
          ? "writing"
          : flash.state === "failed"
            ? "unplugged"
            : "connected"
        : step === 5
          ? "router"
          : step === 6
            ? "done"
            : "unplugged";
  const figure = `/install/${figureState}-${theme}.svg`;

  const common = { t, lang, figure };
  let content;
  if (step === 1)
    content = (
      <BoardStep
        {...common}
        boardId={boardId}
        pickBoard={pickBoard}
        chipFilter={chipFilter}
        setChipFilter={(chip) => {
          setChipFilter(chip);
          setDetected(null);
        }}
        detected={detected}
        clearDetected={() => {
          setDetected(null);
          setChipFilter("all");
        }}
        detecting={detecting}
        detect={supported ? detect : null}
        nextDisabled={!board || (!cli && !supported)}
        next={() => go(2)}
        nextLabel={cli ? t.board.nextCli : t.board.next}
      />
    );
  else if (step === 2 && board && cli)
    content = <CliStep {...common} kind={cli} back={() => go(1)} copy={copy} />;
  else if (step === 2 && board)
    content = (
      <FirmwareStep
        {...common}
        board={board}
        releases={releases.state}
        offered={offered}
        release={release}
        pickRelease={pickRelease}
        retry={loadReleases}
        file={file}
        dropFile={dropFile}
        clearFile={() => setFile({ state: "idle" })}
        back={() => go(1)}
        next={() => go(3)}
      />
    );
  else if (step === 3 && board)
    content = (
      <ConnectStep
        {...common}
        board={board}
        device={device}
        supported={supported}
        versionLabel={versionLabel}
        installedVersion={installedVersion}
        fresh={fresh}
        setFresh={setFresh}
        connect={connect}
        reconnect={async () => {
          await forget();
          setDevice({ state: "idle" });
        }}
        pickByChip={(chip) => {
          void disconnect();
          setChipFilter(chip);
          setDetected(chip);
          setBoardId(null);
          setFile({ state: "idle" });
          go(1);
        }}
        startFlash={startFlash}
        back={async () => {
          await disconnect();
          go(2);
        }}
      />
    );
  else if (step === 4 && pkg)
    content = (
      <FlashStep
        {...common}
        flash={flash}
        pkg={pkg}
        log={log}
        downloadLog={downloadLog}
        reconnect={reconnect}
        next={() => go(5)}
      />
    );
  else if (step === 5)
    content = <WifiStep {...common} wifi={wifi} reopen={reopen} />;
  else
    content = (
      <DoneStep
        {...common}
        wifi={wifi}
        versionLabel={versionLabel}
        copyAddress={(url) => copy(url, t.done.copied)}
        reset={reset}
      />
    );

  return (
    <Frame
      t={t}
      lang={lang}
      theme={theme}
      themePref={themePref}
      chooseTheme={chooseTheme}
      step={step}
      cli={!!cli}
      summary={{
        board: board?.name ?? "—",
        chip: board?.chip ?? "—",
        version: step >= 2 && !cli ? versionLabel : "—",
        device: deviceMac,
      }}
      noSerial={!supported}
      copyLink={() => copy(window.location.href, t.noSerial.copied)}
      toast={toast}
      closeToast={() => setToast(null)}
    >
      {content}
    </Frame>
  );
}

