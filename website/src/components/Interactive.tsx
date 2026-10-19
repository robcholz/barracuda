"use client";

import { Fragment, useState } from "react";
import type { Copy } from "@/content/en";
import { docs, install } from "@/lib/links";
import { Icon } from "./Icon";

const COMMAND = "cargo board select && cargo run";

/** The hero's install box: the browser installer, or the command to build from source. */
export function InstallBox({ copy }: { copy: Copy }) {
  const { hero, lang } = copy;
  const [via, setVia] = useState<"browser" | "source">("browser");
  const [copied, setCopied] = useState(false);
  const browser = via === "browser";
  const pick = (next: typeof via) => {
    setVia(next);
    setCopied(false);
  };
  const copyCommand = async () => {
    try {
      await navigator.clipboard.writeText(COMMAND);
    } catch {}
    setCopied(true);
  };

  return (
    <div className="w-install">
      {browser ? (
        <div className="bc-command">
          <span className="bc-command__note">{hero.browserNote}</span>
          <a className="bc-button" href={install(lang)}>
            {hero.installButton}
          </a>
        </div>
      ) : (
        <div className="bc-command">
          <span className="bc-command__prompt">$</span>
          <span className="bc-command__text">{COMMAND}</span>
          <button
            className="bc-icon-button"
            type="button"
            aria-label={hero.copyLabel}
            onClick={copyCommand}
          >
            <Icon name="copy" />
          </button>
        </div>
      )}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <div
          className="bc-segmented bc-segmented--text"
          style={{ marginTop: 10 }}
          role="group"
          aria-label={hero.startLabel}
        >
          <button
            type="button"
            aria-pressed={browser}
            onClick={() => pick("browser")}
          >
            {hero.tabBrowser}
          </button>
          <button
            type="button"
            aria-pressed={!browser}
            onClick={() => pick("source")}
          >
            {hero.tabSource}
          </button>
        </div>
        <span
          className="bc-small bc-muted"
          style={{ marginTop: 10 }}
          aria-live="polite"
        >
          {browser
            ? hero.noteBrowser
            : copied
              ? hero.noteCopied
              : hero.noteSource}
        </span>
      </div>
    </div>
  );
}

/** The six levels: pick one on the left, see it work on the right. */
export function Tour({ copy }: { copy: Copy }) {
  const steps = copy.ladder.steps;
  const [step, setStep] = useState(0);
  return (
    <div className="w-tour">
      <div className="w-steps">
        {steps.map((s, i) => (
          <button
            key={s.title}
            className="w-stepbtn"
            type="button"
            aria-pressed={i === step}
            onClick={() => setStep(i)}
          >
            <span className="bc-step__mark">
              {String(i + 1).padStart(2, "0")}
            </span>
            <span style={{ fontWeight: 500 }}>{s.title}</span>
            <span
              className="w-stepbtn__cmd"
              style={{ fontFamily: "var(--font-sans)" }}
            >
              {s.hint}
            </span>
            {i === step && <span className="w-stepbtn__desc">{s.desc}</span>}
          </button>
        ))}
      </div>
      <div>
        <div className="bc-window" data-theme="dark">
          <div className="bc-window__bar">
            <span className="bc-window__dots">
              <i />
              <i />
              <i />
            </span>
            <span className="bc-window__title">{steps[step].bar}</span>
          </div>
          <pre className="bc-window__code">{steps[step].out}</pre>
        </div>
      </div>
    </div>
  );
}

/** Board makers: what adding a board takes, one item at a time. */
export function MakerPanel({ copy }: { copy: Copy }) {
  const { makers, lang } = copy;
  const [index, setIndex] = useState(0);
  const item = makers.items[index];
  return (
    <div className="w-api">
      <ul className="w-api__list">
        {makers.items.map((m, i) => (
          <li key={m.label}>
            <button
              className="w-api__item"
              type="button"
              aria-pressed={i === index}
              onClick={() => setIndex(i)}
            >
              {m.label}
            </button>
          </li>
        ))}
      </ul>
      <div className="bc-window">
        <div className="bc-window__bar">
          <span className="bc-window__title">{item.note}</span>
          <a className="bc-link bc-small" href={docs(lang, "boards/overview")}>
            {makers.guide}
          </a>
        </div>
        {item.file ? (
          <>
            <div className="bc-window__path">{item.file}</div>
            <pre className="bc-window__code bc-window__code--wide">
              {item.body}
            </pre>
          </>
        ) : (
          <p
            style={{
              margin: 0,
              padding: "28px 24px",
              fontSize: 18,
              lineHeight: "30px",
              minHeight: 240,
            }}
          >
            {item.body}
          </p>
        )}
      </div>
    </div>
  );
}

/** Questions and answers, one open at a time. */
export function Faq({ copy }: { copy: Copy }) {
  const [open, setOpen] = useState(0);
  return (
    <div className="w-faq">
      {copy.faq.items.map((f, i) => (
        <Fragment key={f.q}>
          <button
            className="w-faq__q"
            type="button"
            aria-expanded={open === i}
            onClick={() => setOpen(open === i ? -1 : i)}
          >
            <Icon name="chevronRight" />
            {f.q}
          </button>
          {open === i && <p className="w-faq__a">{f.a}</p>}
        </Fragment>
      ))}
    </div>
  );
}
