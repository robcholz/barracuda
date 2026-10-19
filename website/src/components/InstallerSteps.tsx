"use client";

import { type ReactNode, useEffect, useRef, useState } from "react";
import qrcode from "qrcode-generator";
import { type Board, BOARDS, CHIPS } from "@/content/boards";
import type { InstallCopy } from "@/content/install-en";
import { joinNetworkPayload, portalUrl } from "@/lib/install/console";
import { type Package, archiveName } from "@/lib/install/package";
import { archiveUrl, type Release } from "@/lib/install/releases";
import { docs, GITHUB, type Lang, page } from "@/lib/links";
import { BrandMark } from "./BrandMark";
import { Icon } from "./Icon";
import type { CliKind, DeviceState, FileState, FlashState, ThemePref, WifiState } from "./Installer";

// SSIDs and addresses are UTF-8; the library's default keeps only the low byte.
qrcode.stringToBytes = (text: string) => Array.from(new TextEncoder().encode(text));

type Common = { t: InstallCopy; lang: Lang; figure: string };

/** A QR code, drawn as one path on the design system's light tile. */
function Qr({ text, size, label }: { text: string; size: number; label: string }) {
  const code = qrcode(0, "M");
  code.addData(text);
  code.make();
  const count = code.getModuleCount();
  let path = "";
  for (let row = 0; row < count; row += 1)
    for (let column = 0; column < count; column += 1)
      if (code.isDark(row, column)) path += `M${column + 2} ${row + 2}h1v1h-1z`;
  return (
    <div className="bc-qr" data-theme="light">
      <svg viewBox={`0 0 ${count + 4} ${count + 4}`} width={size} height={size} role="img" aria-label={label}>
        <path d={path} />
      </svg>
    </div>
  );
}

function Header({ figure, step, total, title, lead }: { figure: string; step: number; total: number; title: string; lead: ReactNode }) {
  return (
    <section className="bc-header">
      <div className="bc-header__text">
        <span className="bc-mono bc-caption bc-muted">
          {step} / {total}
        </span>
        <h1 className="bc-page-title">{title}</h1>
        <p className="bc-lead">{lead}</p>
      </div>
      <div className="bc-header__figure">
        <img className="bc-figure" alt="" src={figure} style={{ maxWidth: 240 }} />
      </div>
    </section>
  );
}

function ErrorAlert({ title, children }: { title: ReactNode; children?: ReactNode }) {
  return (
    <div className="bc-alert bc-alert--error" role="alert">
      <Icon name="alert" />
      <div className="bc-alert__body">
        <div className="bc-alert__title">{title}</div>
        {children}
      </div>
    </div>
  );
}

/** One progress row: a done, running, failed or pending mark, a label and a note. */
function StageRow({ state, label, meta }: { state: "done" | "running" | "failed" | "pending"; label: string; meta?: string }) {
  return (
    <div className="bc-card__row">
      {state === "done" && <Icon name="check" className="bc-success" />}
      {state === "running" && <Icon name="spinner" className="bc-spinner" />}
      {state === "failed" && <Icon name="x" className="bc-destructive" />}
      {state === "pending" && <Icon name="pending" className="bc-muted" />}
      <span className={state === "pending" ? "bc-muted" : ""} style={{ flex: "1 1 auto", minWidth: 0 }}>
        {label}
      </span>
      <span className="bc-mono bc-small bc-muted">{meta ?? ""}</span>
    </div>
  );
}

/** The top bar, the step rail with the install summary, and the step. */
export function Frame({
  t,
  lang,
  theme,
  themePref,
  chooseTheme,
  step,
  cli,
  summary,
  noSerial,
  copyLink,
  toast,
  closeToast,
  children,
}: {
  t: InstallCopy;
  lang: Lang;
  theme: "light" | "dark";
  themePref: ThemePref;
  chooseTheme: (pref: ThemePref) => void;
  step: number;
  cli: boolean;
  summary: { board: string; chip: string; version: string; device: string };
  noSerial: boolean;
  copyLink: () => void;
  toast: string | null;
  closeToast: () => void;
  children: ReactNode;
}) {
  const [langOpen, setLangOpen] = useState(false);
  const menu = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!langOpen) return;
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent ? event.key === "Escape" : !menu.current?.contains(event.target as Node))
        setLangOpen(false);
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", close);
    };
  }, [langOpen]);
  const labels = cli ? t.rail.cliSteps : t.rail.steps;
  const themes: [ThemePref, "sun" | "moon" | "monitor", string][] = [
    ["light", "sun", t.theme.light],
    ["dark", "moon", t.theme.dark],
    ["system", "monitor", t.theme.system],
  ];

  return (
    <div className="bc ob-root" data-theme={theme}>
      <header className="bc-topbar">
        <a className="bc-brand" href={page(lang, "/")}>
          <BrandMark />
          <span className="bc-wordmark">Barracuda</span>
        </a>
        <span className="bc-crumb">/</span>
        <span className="bc-crumb--current">{t.crumb}</span>
        <span style={{ flex: "1 1 auto" }} />
        <a className="bc-button bc-button--ghost bc-button--sm" href={docs(lang, "get-started/installation")}>
          <Icon name="help" />
          <span className="ob-hide-sm">{t.help}</span>
        </a>
        <div className="bc-menu-anchor ob-hide-sm" ref={menu}>
          <button
            className="bc-button bc-button--ghost bc-button--sm"
            type="button"
            aria-haspopup="menu"
            aria-expanded={langOpen}
            onClick={() => setLangOpen(!langOpen)}
          >
            <Icon name="languages" />
            {t.languageName}
            <Icon name="chevronDown" />
          </button>
          {langOpen && (
            <div className="bc-menu" role="menu" aria-label={t.language}>
              <a className="bc-menu-item" role="menuitemradio" aria-checked={lang === "zh"} href={page("zh", "/install/")} lang="zh-Hans">
                <span style={{ flex: 1 }}>简体中文</span>
                {lang === "zh" && <Icon name="check" />}
              </a>
              <a className="bc-menu-item" role="menuitemradio" aria-checked={lang === "en"} href={page("en", "/install/")} lang="en">
                <span style={{ flex: 1 }}>English</span>
                {lang === "en" && <Icon name="check" />}
              </a>
            </div>
          )}
        </div>
        <div className="bc-segmented" role="group" aria-label={t.theme.label}>
          {themes.map(([pref, icon, label]) => (
            <button key={pref} type="button" aria-pressed={themePref === pref} aria-label={label} onClick={() => chooseTheme(pref)}>
              <Icon name={icon} />
            </button>
          ))}
        </div>
      </header>

      <main className="ob-page">
        <nav className="ob-rail" aria-label={t.rail.label}>
          <ol className="bc-stepper bc-stepper--collapse">
            {labels.map((label, index) => {
              const number = index + 1;
              const done = number < step;
              return (
                <li
                  key={label}
                  className={done ? "bc-stepper__item bc-stepper__item--done" : "bc-stepper__item"}
                  aria-current={number === step ? "step" : undefined}
                >
                  <span className="bc-stepper__mark">{done ? <Icon name="check" /> : number}</span>
                  <span className="bc-stepper__label">{label}</span>
                </li>
              );
            })}
          </ol>
          <div className="ob-rail__sep ob-hide-sm" />
          <dl className="bc-kv ob-hide-sm">
            <dt>{t.rail.board}</dt>
            <dd>{summary.board}</dd>
            <dt>{t.rail.chip}</dt>
            <dd className="bc-mono">{summary.chip}</dd>
            <dt>{t.rail.version}</dt>
            <dd className="bc-mono">{summary.version}</dd>
            <dt>{t.rail.device}</dt>
            <dd className="bc-mono">{summary.device}</dd>
          </dl>
        </nav>

        <div className="ob-content">
          {noSerial && (
            <ErrorAlert title={t.noSerial.title}>
              <div className="bc-small">{t.noSerial.body}</div>
              <div className="ob-inline" style={{ marginTop: "var(--space-2)" }}>
                <button className="bc-button bc-button--outline bc-button--sm" type="button" onClick={copyLink}>
                  <Icon name="copy" />
                  {t.noSerial.copy}
                </button>
              </div>
            </ErrorAlert>
          )}
          {children}
        </div>
      </main>

      {toast && (
        <div className="bc-toaster">
          <div className="bc-toast bc-toast--success" role="status">
            <Icon name="circleCheck" className="bc-toast__icon" />
            <div className="bc-toast__body">
              <span className="bc-toast__title">{toast}</span>
            </div>
            <button className="bc-toast__close" type="button" aria-label="×" onClick={closeToast}>
              <Icon name="x" />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

/** 01: pick the Board, by search, chip, or the chip read over USB. */
export function BoardStep({
  t,
  lang,
  figure,
  boardId,
  pickBoard,
  chipFilter,
  setChipFilter,
  detected,
  clearDetected,
  detecting,
  detect,
  nextDisabled,
  next,
  nextLabel,
}: Common & {
  boardId: string | null;
  pickBoard: (id: string) => void;
  chipFilter: string;
  setChipFilter: (chip: string) => void;
  detected: string | null;
  clearDetected: () => void;
  detecting: "idle" | "busy" | "failed";
  detect: (() => void) | null;
  nextDisabled: boolean;
  next: () => void;
  nextLabel: string;
}) {
  const [query, setQuery] = useState("");
  const [chipOpen, setChipOpen] = useState(false);
  const q = query.trim().toLowerCase();
  const matched = BOARDS.filter((board) => chipFilter === "all" || board.chip === chipFilter).filter(
    (board) => !q || `${board.name} ${board.id} ${board.vendor}`.toLowerCase().includes(q),
  );
  const groups: { vendor: string; boards: Board[] }[] = [];
  for (const board of matched) {
    let group = groups.find((entry) => entry.vendor === board.vendor);
    if (!group) groups.push((group = { vendor: board.vendor, boards: [] }));
    group.boards.push(board);
  }
  const options = [
    { value: "all", label: t.board.all, count: String(BOARDS.length) },
    ...CHIPS.map((chip) => ({
      value: chip,
      label: chip,
      count: String(BOARDS.filter((board) => board.chip === chip).length).padStart(2, "0"),
    })),
  ];

  return (
    <>
      <Header figure={figure} step={1} total={6} title={t.board.title} lead={t.board.lead} />
      {detected && (
        <div className="bc-alert bc-alert--compact" role="status">
          <Icon name="check" className="bc-success" />
          <span className="bc-small" style={{ flex: "1 1 auto" }}>
            {t.board.detected(detected)}
          </span>
          <button className="bc-button bc-button--ghost bc-button--sm" type="button" onClick={clearDetected}>
            {t.board.showAll}
          </button>
        </div>
      )}
      {detecting === "failed" && <ErrorAlert title={t.board.detectFailed} />}
      <div className="bc-form">
        <div className="bc-picker bc-picker--flush">
          <div className="bc-picker__head">
            <label className="bc-picker__search">
              <Icon name="search" />
              <input
                className="bc-input"
                type="search"
                placeholder={t.board.placeholder}
                aria-label={t.board.searchLabel}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
              />
            </label>
            <div className="bc-menu-anchor">
              <button
                className="bc-button bc-button--outline"
                type="button"
                aria-haspopup="menu"
                aria-expanded={chipOpen}
                onClick={() => setChipOpen(!chipOpen)}
              >
                {t.board.chip} · {chipFilter === "all" ? t.board.all : chipFilter}
                <Icon name="chevronDown" />
              </button>
              {chipOpen && (
                <div className="bc-menu" role="menu">
                  {options.map((option) => (
                    <button
                      key={option.value}
                      className="bc-menu-item"
                      role="menuitemradio"
                      aria-checked={chipFilter === option.value}
                      type="button"
                      onClick={() => {
                        setChipFilter(option.value);
                        setChipOpen(false);
                      }}
                    >
                      {chipFilter === option.value ? <Icon name="check" /> : <span style={{ width: 16 }} />}
                      {option.label}
                      <span className="bc-mono bc-caption bc-muted" style={{ marginLeft: "auto" }}>
                        {option.count}
                      </span>
                    </button>
                  ))}
                </div>
              )}
            </div>
            {detect && (
              <button className="bc-button bc-button--outline" type="button" disabled={detecting === "busy"} onClick={detect}>
                <Icon name={detecting === "busy" ? "spinner" : "usb"} className={detecting === "busy" ? "bc-spinner" : undefined} />
                {detecting === "busy" ? t.board.detecting : t.board.detect}
              </button>
            )}
            <span className="bc-picker__count">{matched.length}</span>
          </div>
          <div className="bc-picker__list" role="radiogroup" aria-label={t.board.boards}>
            {groups.map((group) => (
              <div key={group.vendor} style={{ display: "contents" }}>
                <p className="bc-picker__group">{group.vendor}</p>
                {group.boards.map((board) => (
                  <label key={board.id} className="bc-picker__row">
                    <input type="radio" name="board" checked={boardId === board.id} onChange={() => pickBoard(board.id)} />
                    <span className="bc-option-title">{board.name}</span>
                    <span className="bc-small bc-muted bc-mono">{board.chip}</span>
                  </label>
                ))}
              </div>
            ))}
            {matched.length === 0 && (
              <p className="bc-picker__note">
                <span>{t.board.none}</span>
                <button
                  className="bc-button bc-button--outline bc-button--sm"
                  type="button"
                  onClick={() => {
                    setQuery("");
                    setChipFilter("all");
                  }}
                >
                  {t.board.clear}
                </button>
              </p>
            )}
            <p className="bc-picker__note">
              <span>{t.board.missing}</span>
              <a className="bc-link" href={docs(lang, "platforms/esp32")}>
                {t.board.platforms}
              </a>
            </p>
          </div>
        </div>
        <div className="bc-form__footer">
          <button className="bc-button" type="button" disabled={nextDisabled} onClick={next}>
            {nextLabel}
          </button>
        </div>
      </div>
    </>
  );
}

const CLI_COMMANDS: Record<CliKind, [keyof InstallCopy["cli"]["steps"], string][]> = {
  stm32: [
    ["toolchain", "rustup toolchain install 1.98.1 --target thumbv8m.main-none-eabihf\ncargo install probe-rs-tools"],
    ["run", "cargo board select nucleo-u5a5zj-q\ncargo run"],
    ["image", "cargo image deploy"],
  ],
  linux: [
    ["network", "sudo sh platforms/linux/provision.sh barracuda0"],
    ["build", "cargo board select local-linux\nBARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run"],
    ["configure", "cargo cli configure"],
    ["chat", "cargo cli"],
  ],
  macos: [
    ["build", "cargo board select local-macos\nBARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run"],
    ["configure", "cargo cli configure"],
    ["chat", "cargo cli"],
  ],
};

/** 02 for STM32 and the host Boards: the commands that install them. */
export function CliStep({
  t,
  lang,
  figure,
  kind,
  back,
  copy,
}: Common & { kind: CliKind; back: () => void; copy: (text: string, done: string) => void }) {
  const doc = kind === "stm32" ? docs(lang, "platforms/stm32") : docs(lang, "get-started/quickstart");
  return (
    <>
      <Header figure={figure} step={2} total={2} title={t.cli.title} lead={t.cli[kind]} />
      <div className="bc-form">
        <div className="ob-body">
          <span className="bc-small bc-muted">
            {t.cli.before}{" "}
            <a className="bc-link" href={docs(lang, "get-started/installation")}>
              {t.cli.installation}
            </a>
            {t.cli.afterLink}
            {t.cli.root}
          </span>
        </div>
        {CLI_COMMANDS[kind].map(([title, code], index) => {
          const lines = code.split("\n");
          const copyButton = (
            <button className="bc-icon-button" type="button" aria-label={t.cli.copy} onClick={() => copy(code, t.cli.copied)}>
              <Icon name="copy" />
              <span className="bc-tooltip" aria-hidden="true">
                {t.cli.copy}
              </span>
            </button>
          );
          return (
            <div className="bc-row" key={title}>
              <div className="bc-row__label">
                <span className="bc-mono bc-caption bc-muted">{index + 1}</span>
                <span className="bc-title">{t.cli.steps[title]}</span>
              </div>
              <div className="bc-row__body">
                {lines.length === 1 ? (
                  <div className="bc-command">
                    <span className="bc-command__prompt">$</span>
                    <span className="bc-command__text">{code}</span>
                    {copyButton}
                  </div>
                ) : (
                  <div className="bc-window">
                    <div className="bc-window__bar">
                      <span className="bc-window__title">{t.cli.terminal}</span>
                      {copyButton}
                    </div>
                    <pre className="bc-window__code">
                      {lines.map((line) => (
                        <span key={line} style={{ display: "block" }}>
                          <span className="bc-window__prompt">$</span> {line}
                        </span>
                      ))}
                    </pre>
                  </div>
                )}
              </div>
            </div>
          );
        })}
        <div className="bc-form__footer">
          <button className="bc-button bc-button--outline" type="button" onClick={back}>
            {t.cli.back}
          </button>
          <a className="bc-button" href={doc}>
            <Icon name="external" />
            {kind === "stm32" ? t.cli.stm32Doc : t.cli.quickstart}
          </a>
        </div>
      </div>
    </>
  );
}

/** 02: pick a version, download its archive, drop it back. */
export function FirmwareStep({
  t,
  figure,
  board,
  releases,
  offered,
  release,
  pickRelease,
  retry,
  file,
  dropFile,
  clearFile,
  back,
  next,
}: Common & {
  board: Board;
  releases: "loading" | "failed" | "ready";
  offered: Release[];
  release: Release | null;
  pickRelease: (tag: string) => void;
  retry: () => void;
  file: FileState;
  dropFile: (file: File) => void;
  clearFile: () => void;
  back: () => void;
  next: () => void;
}) {
  const input = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const url = release ? archiveUrl(release, board.id) : null;
  const name = release ? archiveName(release.version, board.id) : "";
  const problems = t.firmware.problems;
  const problemTitle = (problem: Extract<FileState, { state: "problem" }>["problem"]) => {
    switch (problem.kind) {
      case "unreadable":
        return problems.unreadable;
      case "not-barracuda":
        return problems.notBarracuda;
      case "other-board":
        return problems.otherBoard(BOARDS.find((entry) => entry.id === problem.board)?.name ?? problem.board);
      case "other-version":
        return problems.otherVersion(problem.version);
      case "not-flashable":
        return problems.notFlashable;
      case "corrupt":
        return problems.corrupt(problem.path);
    }
  };

  return (
    <>
      <Header figure={figure} step={2} total={6} title={t.firmware.title} lead={t.firmware.lead(board.name)} />
      <div className="bc-form">
        <div className="bc-row">
          <div className="bc-row__label">
            <span className="bc-title">{t.firmware.version}</span>
            <span className="bc-small bc-muted">{t.firmware.versionHint}</span>
          </div>
          <div className="bc-row__body">
            {releases === "loading" && (
              <span className="bc-status">
                <Icon name="spinner" className="bc-spinner" />
                <span className="bc-shimmer">{t.firmware.loading}</span>
              </span>
            )}
            {releases === "failed" && (
              <ErrorAlert title={t.firmware.loadFailed}>
                <div className="ob-inline" style={{ marginTop: "var(--space-2)" }}>
                  <button className="bc-button bc-button--outline bc-button--sm" type="button" onClick={retry}>
                    {t.firmware.retry}
                  </button>
                </div>
              </ErrorAlert>
            )}
            {releases === "ready" && offered.length === 0 && (
              <span className="bc-small bc-muted">
                {t.firmware.empty}{" "}
                <a className="bc-link" href={`${GITHUB}/releases`}>
                  {t.firmware.releases}
                </a>
              </span>
            )}
            {offered.length > 0 && (
              <div role="radiogroup" aria-label={t.firmware.version} style={{ display: "grid", gap: "var(--space-2)" }}>
                {offered.map((entry) => (
                  <label key={entry.tag} className="bc-radio-card">
                    <input type="radio" name="version" checked={release?.tag === entry.tag} onChange={() => pickRelease(entry.tag)} />
                    <span>
                      <span className="bc-option-title">
                        {entry.prerelease ? t.firmware.prerelease : t.firmware.stable} · <span className="bc-mono">{entry.tag}</span>
                      </span>
                      <span className="bc-hint">{entry.prerelease ? t.firmware.previewHint : t.firmware.recommended}</span>
                    </span>
                  </label>
                ))}
              </div>
            )}
          </div>
        </div>

        {release && (
          <div className="bc-row">
            <div className="bc-row__label">
              <span className="bc-title">{t.firmware.file}</span>
              <span className="bc-small bc-muted">{t.firmware.fileHint}</span>
            </div>
            <div className="bc-row__body" style={{ gap: "var(--space-3)" }}>
              {!url ? (
                <span className="bc-small bc-muted">{t.firmware.notInRelease(board.name)}</span>
              ) : (
                <>
                  <ol className="bc-steps">
                    <li className="bc-step">
                      <span className="bc-step__mark">1</span>
                      <span className="ob-inline">
                        <span>
                          {t.firmware.download("")}
                          <span className="bc-mono" style={{ overflowWrap: "anywhere" }}>
                            {name}
                          </span>
                        </span>
                        <a className="bc-button bc-button--outline bc-button--sm" href={url}>
                          <Icon name="download" />
                          {t.firmware.downloadButton}
                        </a>
                      </span>
                    </li>
                    <li className="bc-step">
                      <span className="bc-step__mark">2</span>
                      <span>{t.firmware.dropStep}</span>
                    </li>
                  </ol>
                  <input
                    ref={input}
                    className="ob-sr"
                    type="file"
                    accept=".gz,.tgz,application/gzip"
                    tabIndex={-1}
                    aria-hidden="true"
                    onChange={(event) => {
                      const picked = event.target.files?.[0];
                      if (picked) dropFile(picked);
                      event.target.value = "";
                    }}
                  />
                  {file.state === "idle" && (
                    <button
                      className="ob-drop"
                      type="button"
                      data-dragging={dragging}
                      onClick={() => input.current?.click()}
                      onDragOver={(event) => {
                        event.preventDefault();
                        setDragging(true);
                      }}
                      onDragLeave={() => setDragging(false)}
                      onDrop={(event) => {
                        event.preventDefault();
                        setDragging(false);
                        const dropped = event.dataTransfer.files[0];
                        if (dropped) dropFile(dropped);
                      }}
                    >
                      <Icon name="upload" className="bc-muted" />
                      <span className="bc-option-title">{t.firmware.drop}</span>
                      <span className="bc-small bc-muted">
                        {t.firmware.dropHint} · <span className="bc-mono">.tar.gz</span>
                      </span>
                    </button>
                  )}
                  {file.state === "checking" && (
                    <div className="ob-file">
                      <Icon name="spinner" className="bc-spinner" />
                      <span className="bc-shimmer" style={{ flex: "1 1 auto", minWidth: 0, overflowWrap: "anywhere" }}>
                        {t.firmware.checking(file.name)}
                      </span>
                    </div>
                  )}
                  {file.state === "ok" && (
                    <div className="ob-file">
                      <Icon name="check" className="bc-success" />
                      <span style={{ display: "flex", flexDirection: "column", flex: "1 1 auto", minWidth: 0 }}>
                        <span className="bc-mono bc-small" style={{ overflowWrap: "anywhere" }}>
                          {file.name}
                        </span>
                        <span className="bc-small bc-muted">
                          {megabytes(file.pkg.bytes)} · {board.name} · <span className="bc-mono">{release.tag}</span> · {t.firmware.checked}
                        </span>
                      </span>
                      <button className="bc-icon-button" type="button" aria-label={t.firmware.remove} onClick={clearFile}>
                        <Icon name="x" />
                      </button>
                    </div>
                  )}
                  {file.state === "problem" && (
                    <ErrorAlert title={problemTitle(file.problem)}>
                      <div className="bc-small">{t.firmware.problemHint(board.name, release.tag)}</div>
                      <div className="ob-inline" style={{ marginTop: "var(--space-2)" }}>
                        <a className="bc-button bc-button--outline bc-button--sm" href={url}>
                          {t.firmware.redownload}
                        </a>
                        <button className="bc-button bc-button--ghost bc-button--sm" type="button" onClick={clearFile}>
                          {t.firmware.another}
                        </button>
                      </div>
                    </ErrorAlert>
                  )}
                </>
              )}
            </div>
          </div>
        )}

        {release && (
          <div className="bc-row">
            <div className="bc-row__label">
              <span className="bc-title">{t.firmware.notes}</span>
              <span className="bc-small bc-muted bc-mono">{release.tag}</span>
            </div>
            <div className="bc-row__body" style={{ gap: "var(--space-3)" }}>
              {release.notes.length === 0 && <span className="bc-small bc-muted">{t.firmware.noNotes}</span>}
              {release.notes.map((note) => (
                <span key={note} style={{ display: "flex", alignItems: "flex-start", gap: "var(--space-2)" }}>
                  <Icon name="plus" className="bc-muted" />
                  {note}
                </span>
              ))}
              <a className="bc-link bc-small" href={release.url}>
                {t.firmware.allNotes}
              </a>
            </div>
          </div>
        )}

        <div className="bc-form__footer">
          <button className="bc-button bc-button--outline" type="button" onClick={back}>
            {t.firmware.back}
          </button>
          <button className="bc-button" type="button" disabled={file.state !== "ok"} onClick={next}>
            {t.firmware.next}
          </button>
        </div>
      </div>
    </>
  );
}

function megabytes(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 03: connect, check the chip, choose update or fresh install. */
export function ConnectStep({
  t,
  figure,
  board,
  device,
  supported,
  versionLabel,
  installedVersion,
  fresh,
  setFresh,
  connect,
  reconnect,
  pickByChip,
  startFlash,
  back,
}: Common & {
  board: Board;
  device: DeviceState;
  supported: boolean;
  versionLabel: string;
  installedVersion: string | null;
  fresh: boolean;
  setFresh: (fresh: boolean) => void;
  connect: () => void;
  reconnect: () => void;
  pickByChip: (chip: string) => void;
  startFlash: () => void;
  back: () => void;
}) {
  const [confirmOpen, setConfirmOpen] = useState(false);
  const before = device.state === "idle" || device.state === "connecting" || device.state === "failed";
  const kv = (chip: { name: string; mac: string; flashSize?: string }, extra?: ReactNode) => (
    <dl className="bc-kv">
      <dt>{t.connect.chip}</dt>
      <dd className="bc-mono">{chip.name}</dd>
      <dt>{t.connect.flash}</dt>
      <dd className="bc-mono">{chip.flashSize ?? "—"}</dd>
      <dt>{t.connect.mac}</dt>
      <dd className="bc-mono">{chip.mac.toUpperCase()}</dd>
      {extra}
    </dl>
  );

  return (
    <>
      <Header figure={figure} step={3} total={6} title={t.connect.title} lead={t.connect.lead} />
      <div className="bc-form">
        {before && (
          <div className="ob-body">
            {device.state === "failed" && (
              <ErrorAlert title={t.connect.failedTitle}>
                <div className="bc-small">{t.connect.failedBody}</div>
              </ErrorAlert>
            )}
            <ol className="bc-steps">
              <li className="bc-step">
                <span className="bc-step__mark">1</span>
                <span>
                  <span className="bc-term" tabIndex={0}>
                    {t.connect.cable(board.name)}
                    <span className="bc-tooltip bc-tooltip--start" role="tooltip">
                      {t.connect.cableTip}
                    </span>
                  </span>
                </span>
              </li>
              <li className="bc-step">
                <span className="bc-step__mark">2</span>
                <span>{t.connect.click}</span>
              </li>
              <li className="bc-step">
                <span className="bc-step__mark">3</span>
                <span>{t.connect.pick}</span>
              </li>
            </ol>
            {device.state === "connecting" && (
              <span className="bc-status">
                <Icon name="spinner" className="bc-spinner" />
                <span className="bc-shimmer">{t.connect.connecting}</span>
              </span>
            )}
          </div>
        )}

        {device.state === "connected" && (
          <>
            <div className="bc-row">
              <div className="bc-row__label">
                <span className="bc-title">{t.connect.device}</span>
                <span className="bc-badge bc-badge--signal" style={{ alignSelf: "flex-start" }}>
                  {t.connect.connected}
                </span>
              </div>
              <div className="bc-row__body">
                {kv(
                  device.chip,
                  <>
                    <dt>{t.connect.installed}</dt>
                    <dd>
                      <span className="bc-mono">{installedVersion ?? t.connect.other}</span>
                    </dd>
                  </>,
                )}
              </div>
            </div>
            <div className="bc-row">
              <div className="bc-row__label">
                <span className="bc-title">{t.connect.mode}</span>
                <span className="bc-small bc-muted">
                  {device.installed.sameLayout ? t.connect.modeHasBarracuda : t.connect.modeNoBarracuda}
                </span>
              </div>
              <div className="bc-row__body">
                <div className="ob-pair" role="radiogroup" aria-label={t.connect.mode}>
                  <label className="bc-radio-card">
                    <input
                      type="radio"
                      name="install"
                      checked={!fresh}
                      disabled={!device.installed.sameLayout}
                      onChange={() => {
                        setFresh(false);
                        setConfirmOpen(false);
                      }}
                    />
                    <span>
                      <span className="bc-option-title">
                        {t.connect.update("")}
                        <span className="bc-mono">{versionLabel}</span>
                      </span>
                      <span className="bc-hint">{t.connect.updateHint}</span>
                    </span>
                  </label>
                  <label className="bc-radio-card">
                    <input type="radio" name="install" checked={fresh} onChange={() => setFresh(true)} />
                    <span>
                      <span className="bc-option-title">{t.connect.fresh}</span>
                      <span className="bc-hint">{t.connect.freshHint}</span>
                    </span>
                  </label>
                </div>
              </div>
            </div>
          </>
        )}

        {(device.state === "mismatch" || device.state === "small") && (
          <div className="ob-body">
            {device.state === "mismatch" ? (
              <ErrorAlert title={t.connect.mismatchTitle(device.chip.name)}>
                <div className="bc-small">{t.connect.mismatchBody}</div>
                <div className="ob-inline" style={{ marginTop: "var(--space-2)" }}>
                  {CHIPS.includes(device.chip.name) && (
                    <button className="bc-button bc-button--outline bc-button--sm" type="button" onClick={() => pickByChip(device.chip.name)}>
                      {t.connect.pickByChip(device.chip.name)}
                    </button>
                  )}
                  <button className="bc-button bc-button--ghost bc-button--sm" type="button" onClick={reconnect}>
                    {t.connect.reconnect}
                  </button>
                </div>
              </ErrorAlert>
            ) : (
              <ErrorAlert title={t.connect.smallTitle(device.chip.flashSize ?? "—", device.needed)}>
                <div className="bc-small">{t.connect.smallBody}</div>
                <div className="ob-inline" style={{ marginTop: "var(--space-2)" }}>
                  <button className="bc-button bc-button--ghost bc-button--sm" type="button" onClick={reconnect}>
                    {t.connect.reconnect}
                  </button>
                </div>
              </ErrorAlert>
            )}
            {kv(device.chip)}
          </div>
        )}

        <div className="bc-form__footer">
          <button className="bc-button bc-button--outline" type="button" onClick={back}>
            {t.connect.back}
          </button>
          {before && (
            <button className="bc-button" type="button" disabled={!supported || device.state === "connecting"} onClick={connect}>
              <Icon name="usb" />
              {t.connect.button}
            </button>
          )}
          {device.state === "connected" && !fresh && (
            <button className="bc-button" type="button" onClick={startFlash}>
              {t.connect.write(versionLabel)}
            </button>
          )}
          {device.state === "connected" && fresh && (
            <div className="bc-menu-anchor">
              <button
                className="bc-button bc-button--danger"
                type="button"
                aria-haspopup="dialog"
                aria-expanded={confirmOpen}
                onClick={() => setConfirmOpen(!confirmOpen)}
              >
                {t.connect.wipe}
              </button>
              {confirmOpen && (
                <div
                  className="bc-menu bc-menu--confirm"
                  role="alertdialog"
                  aria-labelledby="ob-confirm-title"
                  style={{ right: 0, bottom: "calc(100% + 8px)", top: "auto", left: "auto" }}
                >
                  <span style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                    <span id="ob-confirm-title" className="bc-option-title">
                      {t.connect.confirmTitle}
                    </span>
                    <span className="bc-small bc-muted">{t.connect.confirmBody(board.name)}</span>
                  </span>
                  <span style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
                    <button className="bc-button bc-button--outline bc-button--sm" type="button" onClick={() => setConfirmOpen(false)}>
                      {t.connect.cancel}
                    </button>
                    <button className="bc-button bc-button--danger bc-button--sm" type="button" onClick={startFlash}>
                      {t.connect.wipe}
                    </button>
                  </span>
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </>
  );
}

/** 04: the write, stage by stage, with the log. */
export function FlashStep({
  t,
  figure,
  flash,
  pkg,
  log,
  downloadLog,
  reconnect,
  next,
}: Common & {
  flash: FlashState;
  pkg: Package;
  log: string[];
  downloadLog: () => void;
  reconnect: () => void;
  next: () => void;
}) {
  const [logOpen, setLogOpen] = useState(false);
  const open = logOpen || flash.state === "failed";
  const s = t.flash.stages;
  // The firmware's share of the write decides where its stage ends.
  const firmwareShare = (pkg.parts[0]?.data.length ?? 0) / pkg.bytes;
  const firmwareEnd = 14 + Math.round(80 * firmwareShare);
  const stages = [
    { label: s.verify, from: 0, to: 4, note: "sha256" },
    { label: flash.fresh ? s.eraseAll : s.eraseApp, from: 4, to: 14, note: flash.fresh ? "" : t.flash.keep },
    { label: s.firmware, from: 14, to: firmwareEnd, note: "" },
    { label: s.resources, from: firmwareEnd, to: 94, note: "" },
    { label: s.restart, from: 94, to: 100, note: "" },
  ];
  const pct = flash.pct;
  const rows = stages.map((stage) => {
    const failed = flash.state === "failed" && pct >= stage.from && pct < stage.to;
    const done = flash.state === "done" || pct >= stage.to;
    const running = !failed && !done && pct >= stage.from && flash.state === "running";
    const within = Math.round(((pct - stage.from) / Math.max(1, stage.to - stage.from)) * 100);
    return {
      label: stage.label,
      state: (failed ? "failed" : done ? "done" : running ? "running" : "pending") as "failed" | "done" | "running" | "pending",
      meta: failed ? t.flash.stopped : running ? `${within}%` : done ? stage.note : "",
    };
  });
  const current = rows.find((row) => row.state === "running");
  const pctText = `${pct}%`;

  return (
    <>
      <Header figure={figure} step={4} total={6} title={t.flash.title} lead={t.flash.lead} />
      {flash.state === "failed" && (
        <ErrorAlert title={t.flash.failedTitle(pctText)}>
          <div className="bc-small">{t.flash.failedBody}</div>
        </ErrorAlert>
      )}
      <div className={flash.state === "running" ? "bc-card bc-card--progress" : "bc-card"} style={{ ["--progress" as string]: pctText }}>
        <div className="bc-card__body" style={{ display: "flex", flexDirection: "column", gap: "var(--space-1)" }}>
          <div style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: "var(--space-4)" }}>
            <span className={flash.state === "running" ? "bc-title bc-shimmer" : "bc-title"}>
              {flash.state === "done" ? t.flash.done : flash.state === "failed" ? t.flash.failedTitle(pctText) : t.flash.working(current?.label ?? s.firmware)}
            </span>
            <span className="bc-mono" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-label={t.flash.progress}>
              {pctText}
            </span>
          </div>
          <span className="bc-small bc-muted">
            {flash.state === "done"
              ? t.flash.doneNote
              : flash.state === "failed"
                ? t.flash.failedNote
                : t.flash.written(megabytes(flash.written), megabytes(pkg.bytes))}
          </span>
        </div>
        {rows.map((row) => (
          <StageRow key={row.label} state={row.state} label={row.label} meta={row.meta} />
        ))}
        <div className="bc-card__row">
          <button className="bc-disclosure" type="button" aria-expanded={open} onClick={() => setLogOpen(!open)}>
            <Icon name="chevronRight" />
            {t.flash.log}
          </button>
        </div>
        <div className="bc-card__foot">
          <span style={{ flex: "1 1 auto" }} />
          {flash.state === "running" && (
            <button className="bc-button" type="button" disabled>
              {t.flash.writing}
            </button>
          )}
          {flash.state === "failed" && (
            <>
              <button className="bc-button bc-button--outline" type="button" onClick={downloadLog}>
                <Icon name="download" />
                {t.flash.downloadLog}
              </button>
              <button className="bc-button" type="button" onClick={reconnect}>
                {t.flash.reconnect}
              </button>
            </>
          )}
          {flash.state === "done" && (
            <button className="bc-button" type="button" onClick={next}>
              {t.flash.next}
            </button>
          )}
        </div>
      </div>
      {open && (
        <div className="bc-window">
          <div className="bc-window__bar">
            <span className="bc-window__title">{t.flash.logTitle}</span>
          </div>
          <pre className="bc-window__code">{log.slice(-200).join("\n")}</pre>
        </div>
      )}
    </>
  );
}

/** 05: join the hotspot by phone; the console says when the Board is online. */
export function WifiStep({ t, figure, wifi, reopen }: Common & { wifi: WifiState; reopen: () => void }) {
  const [manualOpen, setManualOpen] = useState(false);
  const net = wifi.net ?? t.wifi.yourNetwork;
  const rows: { label: string; state: "done" | "running" | "failed" | "pending"; meta?: string }[] = [
    { label: t.wifi.hotspotOn, state: "done", meta: wifi.ap },
    { label: wifi.state === "waiting" ? t.wifi.waiting : t.wifi.received, state: wifi.state === "waiting" ? "running" : "done" },
    {
      label: t.wifi.joining(net),
      state: wifi.state === "joining" ? "running" : wifi.state === "failed" ? "failed" : wifi.state === "online" ? "done" : "pending",
      meta: wifi.state === "failed" ? t.wifi.failedMeta : "",
    },
    { label: t.wifi.address, state: wifi.state === "online" ? "done" : "pending", meta: wifi.ip ?? "" },
  ];

  return (
    <>
      <Header
        figure={figure}
        step={5}
        total={6}
        title={t.wifi.title}
        lead={t.wifi.lead}
      />
      {wifi.state === "failed" && (
        <ErrorAlert title={t.wifi.failedTitle(net)}>
          <div className="bc-small">{t.wifi.failedBody}</div>
        </ErrorAlert>
      )}
      {wifi.lost && (
        <ErrorAlert title={t.wifi.lost}>
          <div className="ob-inline" style={{ marginTop: "var(--space-2)" }}>
            <button className="bc-button bc-button--outline bc-button--sm" type="button" onClick={reopen}>
              <Icon name="usb" />
              {t.wifi.reopen}
            </button>
          </div>
        </ErrorAlert>
      )}
      <div className="bc-card">
        <div className="bc-card__body ob-hotspot">
          <Qr text={joinNetworkPayload(wifi.ap)} size={164} label={t.wifi.qrLabel(wifi.ap)} />
          <ol className="bc-steps" style={{ flex: "1 1 280px", minWidth: 0 }}>
            <li className="bc-step">
              <span className="bc-step__mark">1</span>
              <span>
                {t.wifi.scan}
                <span className="bc-term" tabIndex={0}>
                  {t.wifi.hotspot}
                  <span className="bc-tooltip bc-tooltip--start" role="tooltip">
                    {t.wifi.hotspotTip}
                  </span>
                </span>{" "}
                <span className="bc-mono">{wifi.ap}</span>
              </span>
            </li>
            <li className="bc-step">
              <span className="bc-step__mark">2</span>
              <span>{t.wifi.choose}</span>
            </li>
            <li className="bc-step">
              <span className="bc-step__mark">3</span>
              <span>{t.wifi.back}</span>
            </li>
          </ol>
        </div>
        {rows.map((row) => (
          <StageRow key={row.label} {...row} />
        ))}
        <div className="bc-card__row">
          <button className="bc-disclosure" type="button" aria-expanded={manualOpen} onClick={() => setManualOpen(!manualOpen)}>
            <Icon name="chevronRight" />
            {t.wifi.manual}
          </button>
        </div>
        {manualOpen && (
          <div className="bc-card__body">
            <ol className="bc-steps">
              <li className="bc-step">
                <span className="bc-step__mark">1</span>
                <span className="bc-term" tabIndex={0}>
                  {t.wifi.manualJoin(wifi.ap)}
                  <span className="bc-tooltip bc-tooltip--start" role="tooltip">
                    {t.wifi.manualTip}
                  </span>
                </span>
              </li>
              <li className="bc-step">
                <span className="bc-step__mark">2</span>
                <span>
                  {t.wifi.manualOpen} <span className="bc-mono">http://192.168.4.1/portal/</span>
                </span>
              </li>
              <li className="bc-step">
                <span className="bc-step__mark">3</span>
                <span>{t.wifi.manualChoose}</span>
              </li>
            </ol>
          </div>
        )}
        <div className="bc-card__foot">
          <span style={{ flex: "1 1 auto" }} />
          <button className="bc-button" type="button" disabled>
            {t.wifi.waitingButton}
          </button>
        </div>
      </div>
    </>
  );
}

/** 06: the Board's portal address on the visitor's network. */
export function DoneStep({
  t,
  figure,
  wifi,
  versionLabel,
  copyAddress,
  reset,
}: Common & {
  wifi: WifiState;
  versionLabel: string;
  copyAddress: (url: string) => void;
  reset: () => void;
}) {
  const url = wifi.ip ? portalUrl(wifi.ip, wifi.port) : "";
  return (
    <>
      <Header figure={figure} step={6} total={6} title={t.done.title} lead={t.done.lead} />
      <div className="bc-card">
        <div className="bc-card__body ob-split">
          <dl className="bc-kv">
            <dt>{t.done.status}</dt>
            <dd>
              <span className="bc-badge bc-badge--signal">{t.done.online}</span>
            </dd>
            <dt>{t.done.network}</dt>
            <dd className="bc-mono">{wifi.net ?? "—"}</dd>
            <dt>{t.done.address}</dt>
            <dd style={{ display: "flex", alignItems: "center", gap: "var(--space-2)" }}>
              <span className="bc-mono" style={{ minWidth: 0, overflowWrap: "anywhere" }}>
                {url}
              </span>
              <button className="bc-icon-button" type="button" aria-label={t.done.copyAddress} onClick={() => copyAddress(url)}>
                <Icon name="copy" />
                <span className="bc-tooltip" aria-hidden="true">
                  {t.done.copyAddress}
                </span>
              </button>
            </dd>
            <dt>{t.done.firmware}</dt>
            <dd className="bc-mono">{versionLabel}</dd>
            <dt>{t.done.connection}</dt>
            <dd>
              <span className="bc-small bc-muted">
                HTTP ·{" "}
                <span className="bc-term" tabIndex={0}>
                  {t.done.plain}
                  <span className="bc-tooltip" role="tooltip">
                    {t.done.plainTip}
                  </span>
                </span>
              </span>
            </dd>
          </dl>
          {url && <Qr text={url} size={132} label={t.done.qrLabel} />}
        </div>
        <div className="bc-card__foot">
          <a className="bc-button" href={url}>
            <Icon name="external" />
            {t.done.open}
          </a>
          <button className="bc-button bc-button--outline" type="button" onClick={() => copyAddress(url)}>
            {t.done.copyAddress}
          </button>
          <span style={{ flex: "1 1 auto" }} />
          <button className="bc-button bc-button--ghost" type="button" onClick={reset}>
            {t.done.another}
          </button>
        </div>
      </div>
      <ol className="bc-grid" style={{ gridTemplateColumns: "repeat(3, minmax(0, 1fr))" }}>
        {t.done.next.map(([title, action], index) => (
          <li className="bc-grid__cell" key={title}>
            <span className="bc-mono bc-caption bc-muted">{index + 1}</span>
            <span className="bc-title">{title}</span>
            {index === 0 ? (
              <span className="bc-status bc-success">
                <Icon name="check" />
                {action} <span className="bc-mono">{wifi.net ?? ""}</span>
              </span>
            ) : (
              <a className="bc-link bc-small" href={url}>
                {action}
              </a>
            )}
          </li>
        ))}
      </ol>
    </>
  );
}
