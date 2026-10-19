"use client";

import { useEffect, useRef, useState } from "react";
import type { Copy } from "@/content/en";
import { GITHUB, docs, install, page } from "@/lib/links";
import { BrandMark } from "./BrandMark";
import { Icon } from "./Icon";

const BANNER_KEY = "barracuda-banner-closed";

/** The banner, then the top bar with its language menu. */
export function Chrome({
  copy,
  current,
}: {
  copy: Copy;
  current: "home" | "boards";
}) {
  const { lang } = copy;
  const [bannerOpen, setBannerOpen] = useState(true);
  const [langOpen, setLangOpen] = useState(false);
  const menu = useRef<HTMLDivElement>(null);

  // A closed banner stays closed for that visitor.
  useEffect(() => {
    try {
      if (localStorage.getItem(BANNER_KEY) === "1") setBannerOpen(false);
    } catch {}
  }, []);

  useEffect(() => {
    if (!langOpen) return;
    const close = (event: Event) => {
      if (
        event instanceof KeyboardEvent
          ? event.key === "Escape"
          : !menu.current?.contains(event.target as Node)
      )
        setLangOpen(false);
    };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", close);
    };
  }, [langOpen]);

  const closeBanner = () => {
    setBannerOpen(false);
    try {
      localStorage.setItem(BANNER_KEY, "1");
    } catch {}
  };
  const here = current === "home" ? "/" : "/boards/";
  const home = page(lang, "/");

  return (
    <>
      {bannerOpen && (
        <div className="bc-banner" role="region" aria-label={copy.banner.label}>
          <span>{copy.banner.text}</span>
          <a href={install(lang)}>{copy.banner.link}</a>
          <button
            className="bc-banner__close"
            type="button"
            aria-label={copy.banner.dismiss}
            onClick={closeBanner}
          >
            <Icon name="x" />
          </button>
        </div>
      )}
      <header className="w-top">
        <div className="w-wrap">
          <nav className="w-nav" aria-label={copy.nav.label}>
            <a className="bc-brand" href={home}>
              <BrandMark />
              <span className="bc-wordmark">Barracuda</span>
            </a>
            <div className="w-nav__links">
              <a
                className="bc-button bc-button--ghost bc-button--sm"
                href={`${home}#product`}
              >
                {copy.nav.product}
              </a>
              <a
                className="bc-button bc-button--ghost bc-button--sm"
                href={`${home}#ladder`}
              >
                {copy.nav.how}
              </a>
              <a
                className="bc-button bc-button--ghost bc-button--sm"
                href={`${home}#plugins`}
              >
                {copy.nav.plugins}
              </a>
              <a
                className="bc-button bc-button--ghost bc-button--sm"
                href={page(lang, "/boards/")}
                aria-current={current === "boards" ? "page" : undefined}
              >
                {copy.nav.boards}
              </a>
              <a
                className="bc-button bc-button--ghost bc-button--sm"
                href={docs(lang)}
              >
                {copy.nav.docs}
              </a>
            </div>
            <span style={{ flex: "1 1 auto" }} />
            <div className="bc-menu-anchor w-hide-sm" ref={menu}>
              <button
                className="bc-button bc-button--ghost bc-button--sm"
                type="button"
                aria-haspopup="menu"
                aria-expanded={langOpen}
                onClick={() => setLangOpen(!langOpen)}
              >
                <Icon name="languages" />
                {copy.nav.languageName}
                <Icon name="chevronDown" />
              </button>
              {langOpen && (
                <div
                  className="bc-menu"
                  role="menu"
                  aria-label={copy.nav.language}
                >
                  <a
                    className="bc-menu-item"
                    role="menuitemradio"
                    aria-checked={lang === "zh"}
                    href={page("zh", here)}
                    lang="zh-Hans"
                  >
                    <span style={{ flex: 1 }}>简体中文</span>
                    {lang === "zh" && <Icon name="check" />}
                  </a>
                  <a
                    className="bc-menu-item"
                    role="menuitemradio"
                    aria-checked={lang === "en"}
                    href={page("en", here)}
                    lang="en"
                  >
                    <span style={{ flex: 1 }}>English</span>
                    {lang === "en" && <Icon name="check" />}
                  </a>
                </div>
              )}
            </div>
            <a
              className="bc-button bc-button--outline bc-button--sm w-hide-sm"
              href={GITHUB}
            >
              <Icon name="github" />
              GitHub
            </a>
            <a className="bc-button bc-button--sm" href={install(lang)}>
              {copy.nav.getStarted}
            </a>
          </nav>
        </div>
      </header>
    </>
  );
}
