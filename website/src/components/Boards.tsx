"use client";

import { type ReactNode, useState } from "react";
import { BOARDS, CHIPS } from "@/content/boards";
import type { Copy } from "@/content/en";
import { docs, page } from "@/lib/links";
import { Chrome } from "./Chrome";
import { Icon } from "./Icon";

/** Every Board: search and filter in the hero, the list below, and the call to Board makers. */
export function Boards({ copy, footer }: { copy: Copy; footer: ReactNode }) {
  const { boards: t, lang } = copy;
  const [query, setQuery] = useState("");
  const [chip, setChip] = useState("all");
  const q = query.trim().toLowerCase();
  const rows = BOARDS.filter((b) => chip === "all" || b.chip === chip).filter(
    (b) => !q || `${b.name} ${b.id} ${b.vendor}`.toLowerCase().includes(q),
  );
  const options = [
    { value: "all", label: t.all, count: String(BOARDS.length) },
    ...CHIPS.map((c) => ({
      value: c,
      label: c,
      count: String(BOARDS.filter((b) => b.chip === c).length).padStart(2, "0"),
    })),
  ];
  const clear = () => {
    setQuery("");
    setChip("all");
  };

  return (
    <div className="w w-boards">
      <Chrome copy={copy} current="boards" />
      <section className="w-band w-hero" data-theme="light">
        <div className="w-wrap">
          <div className="w-center" style={{ padding: "96px 0 88px", gap: 24 }}>
            <h1 className="w-display">
              <span className="w-hl">{BOARDS.length}</span> {t.count}
            </h1>
            <p className="w-lead" style={{ maxWidth: 560 }}>
              {t.lead}
            </p>
            <label className="w-search">
              <span
                style={{
                  position: "absolute",
                  width: 1,
                  height: 1,
                  overflow: "hidden",
                  clipPath: "inset(50%)",
                }}
              >
                {t.searchLabel}
              </span>
              <Icon name="search" />
              <input
                type="search"
                placeholder={t.placeholder}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            <div className="w-chips" role="group" aria-label={t.filterLabel}>
              {options.map((o) => (
                <button
                  key={o.value}
                  className="w-chip"
                  type="button"
                  aria-pressed={chip === o.value}
                  onClick={() => setChip(o.value)}
                >
                  {o.label}
                  <span className="w-chip__n">{o.count}</span>
                </button>
              ))}
            </div>
          </div>
        </div>
      </section>

      <section
        className="w-band"
        data-theme="light"
        style={{ padding: "64px 0 112px" }}
      >
        <div className="w-wrap">
          <div
            style={{
              display: "flex",
              justifyContent: "space-between",
              alignItems: "baseline",
              marginBottom: 16,
            }}
          >
            <span className="bc-mono bc-caption bc-muted">
              {chip === "all" ? t.allChips : chip}
            </span>
            <span className="bc-small bc-muted">
              <span className="bc-mono">
                {String(rows.length).padStart(2, "0")}
              </span>{" "}
              {t.shown}
            </span>
          </div>
          <div className="w-panel">
            <span className="w-screw w-screw--tl" />
            <span className="w-screw w-screw--tr" />
            <span className="w-screw w-screw--bl" />
            <span className="w-screw w-screw--br" />
            <div className="w-row w-row--head">
              <span>{t.head[0]}</span>
              <span className="w-hide-sm">{t.head[1]}</span>
              <span>{t.head[2]}</span>
              <span className="w-hide-sm">{t.head[3]}</span>
              <span />
            </div>
            {rows.map((b) => (
              <a
                className="w-row"
                href={docs(lang, `boards/supported/${b.id}`)}
                key={b.id}
              >
                <span style={{ fontWeight: 500 }}>{b.name}</span>
                <span className="bc-muted w-hide-sm">{b.vendor}</span>
                <span className="bc-mono bc-small">{b.chip}</span>
                <span className="bc-mono bc-small bc-muted w-hide-sm">
                  {b.id}
                </span>
                <span className="w-row__go">
                  {t.go}
                  <Icon name="chevronRight" />
                </span>
              </a>
            ))}
            {rows.length === 0 && (
              <div
                className="w-center"
                style={{
                  gap: 12,
                  padding: "64px 24px",
                  borderTop: "1px solid var(--border)",
                }}
              >
                <span style={{ fontSize: 18, fontWeight: 500 }}>
                  {t.emptyTitle}
                </span>
                <span className="bc-muted">{t.emptyBody}</span>
                <button
                  className="bc-button bc-button--outline bc-button--sm"
                  type="button"
                  onClick={clear}
                >
                  {t.clear}
                </button>
              </div>
            )}
          </div>
          <div className="w-cta">
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: 8,
                maxWidth: 560,
              }}
            >
              <h2>{t.ctaTitle}</h2>
              <span>{t.ctaBody}</span>
            </div>
            <a
              className="bc-button w-btn-lg"
              href={`${page(lang, "/")}#makers`}
            >
              {t.ctaButton}
            </a>
          </div>
        </div>
      </section>
      {footer}
    </div>
  );
}
