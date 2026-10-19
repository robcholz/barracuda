import type { Copy } from "@/content/en";
import { RELEASES, docs, install, page } from "@/lib/links";
import { Chrome } from "./Chrome";
import { Footer } from "./Footer";
import { Icon } from "./Icon";
import { Faq, InstallBox, MakerPanel, Tour } from "./Interactive";

const LOGOS = [
  ["espressif", "Espressif Systems"],
  ["m5stack", "M5Stack"],
  ["st", "STMicroelectronics"],
  ["unexpectedmaker", "Unexpected Maker"],
  ["anthropic", "Anthropic"],
  ["openai", "OpenAI"],
  ["moonshot", "Moonshot AI"],
] as const;

function Dots() {
  return (
    <span className="bc-window__dots">
      <i />
      <i />
      <i />
    </span>
  );
}

export function Home({ copy }: { copy: Copy }) {
  const {
    lang,
    hero,
    product,
    audiences,
    ladder,
    channels,
    plugins,
    makers,
    education,
    faq,
    updates,
    closing,
  } = copy;
  const audienceHref = (to: Copy["audiences"]["items"][number]["to"]) =>
    to === "develop"
      ? docs(lang, "develop/write-a-plugin")
      : to === "install"
        ? install(lang)
        : to;

  return (
    <div className="w">
      <Chrome copy={copy} current="home" />

      {/* Hero */}
      <section className="w-band w-hero" data-theme="light">
        <div className="w-wrap">
          <div className="w-center" style={{ paddingTop: 112, gap: 28 }}>
            <h1 className="w-display">{hero.title}</h1>
            <p className="w-lead" style={{ maxWidth: 660 }}>
              {hero.lead}
            </p>
            <InstallBox copy={copy} />
            <a className="bc-link" href={docs(lang)}>
              {hero.readDocs}
            </a>
          </div>
        </div>
        <div className="w-brands" role="group" aria-label={hero.brandsLabel}>
          <div className="w-brands__view">
            <div className="w-brands__track">
              {[0, 1].map((set) => (
                <div
                  className="w-brands__set"
                  key={set}
                  aria-hidden={set === 1 || undefined}
                >
                  {LOGOS.map(([id, name]) => (
                    <span
                      key={id}
                      className={`w-logo w-logo--${id}`}
                      role={set === 0 ? "img" : undefined}
                      aria-label={set === 0 ? name : undefined}
                    />
                  ))}
                </div>
              ))}
            </div>
          </div>
        </div>
      </section>

      {/* Product: live hardware, plugins, any chip */}
      <section
        className="w-band w-section"
        id="product"
        data-theme="light"
        style={{ paddingBottom: 32 }}
      >
        <div className="w-wrap">
          <div className="w-head" style={{ marginBottom: 32 }}>
            <div className="w-head__l">
              <h2 className="w-h2">{product.title}</h2>
            </div>
            <p className="w-lead" style={{ maxWidth: 380 }}>
              {product.lead}
            </p>
          </div>

          <div className="w-show">
            <div className="w-stage w-stage--ink">
              <div
                className="bc-window"
                data-theme="light"
                style={{ maxWidth: 560 }}
              >
                <div className="bc-window__bar">
                  <Dots />
                  <span className="bc-window__title">{product.chatTitle}</span>
                </div>
                <div
                  style={{
                    padding: 18,
                    display: "flex",
                    flexDirection: "column",
                    gap: 14,
                  }}
                >
                  <div className="bc-turn bc-turn--user">
                    <p className="bc-bubble">{product.chatAsk}</p>
                  </div>
                  <div className="bc-turn">
                    <span className="bc-meta bc-meta--author">Barracuda</span>
                    <div className="bc-card">
                      <button
                        className="bc-fold"
                        type="button"
                        aria-expanded="true"
                      >
                        <Icon name="terminal" className="bc-muted" />
                        <span className="bc-tool__name">vm_run</span>
                        <span className="bc-tool__args">
                          gpio · imu · display
                        </span>
                        <span className="bc-status bc-success">
                          <Icon name="check" />
                          {product.running}
                        </span>
                      </button>
                      <pre className="bc-card__body w-cardcode">
                        {`local button = gpio.open_input("BTN_A", "up")
local sensor, screen = imu.open(), display.open()
while true do
  if not button:read() then show(sensor, screen) end
end`}
                      </pre>
                    </div>
                    <p className="bc-reply">{product.chatReply}</p>
                  </div>
                </div>
              </div>
            </div>
            <div className="w-copy">
              <h3>
                {product.liveTitle}
                <br />
                <span className="w-dim">{product.liveBody}</span>
              </h3>
              <a className="bc-link" href={docs(lang, "use/scripts")}>
                {product.liveLink}
              </a>
            </div>
          </div>

          <div className="w-show w-show--flip">
            <div className="w-stage w-stage--lime">
              <div className="bc-window" data-theme="light">
                <div className="bc-window__bar">
                  <Dots />
                  <span className="bc-window__title">
                    {product.firmwareTitle}
                  </span>
                </div>
                <div className="w-plug">
                  {product.plugs.map(([name, note, on]) => (
                    <div className="w-plug__row" key={name}>
                      <span className="w-plug__name">{name}</span>
                      <span className="bc-small bc-muted">{note}</span>
                      <span
                        className="bc-switch"
                        aria-hidden="true"
                        aria-checked={on}
                      />
                    </div>
                  ))}
                  <div className="w-plug__foot bc-small bc-muted">
                    {product.plugFoot}
                  </div>
                </div>
              </div>
            </div>
            <div className="w-copy">
              <h3>
                {product.plugTitle}
                <br />
                <span className="w-dim">{product.plugBody}</span>
              </h3>
              <a className="bc-link" href="#plugins">
                {product.plugLink}
              </a>
            </div>
          </div>

          <div className="w-show">
            <div className="w-stage w-stage--grid">
              <div
                className="bc-window"
                data-theme="dark"
                style={{ maxWidth: 560 }}
              >
                <div className="bc-window__bar">
                  <Dots />
                  <span className="bc-window__title">{product.termTitle}</span>
                </div>
                <pre className="bc-window__code">
                  {[
                    ["m5stack-cores3", product.termEspressif],
                    ["nucleo-u5a5zj-q", product.termSt],
                    ["local-macos", product.termLaptop],
                  ].map(([board, chip]) => (
                    <span key={board}>
                      <span className="bc-window__prompt">$</span> cargo board
                      select {board}
                      {"\n"}
                      <span className="bc-muted">{`  ${chip}`}</span>
                      {"  "}
                      <span className="bc-success">{product.termBuilt}</span>
                      {"\n\n"}
                    </span>
                  ))}
                  <span className="bc-muted">{product.termSame}</span>
                </pre>
              </div>
            </div>
            <div className="w-copy">
              <h3>
                {product.vendorTitle}
                <br />
                <span className="w-dim">{product.vendorBody}</span>
              </h3>
              <a className="bc-link" href={page(lang, "/boards/")}>
                {product.vendorLink}
              </a>
            </div>
          </div>
        </div>
      </section>

      {/* Who it is for */}
      <section className="w-band w-section" data-theme="light">
        <div className="w-wrap">
          <div className="w-head">
            <div className="w-head__l">
              <h2 className="w-h2">{audiences.title}</h2>
            </div>
          </div>
          <div className="w-tools">
            {audiences.items.map((a) => (
              <div className="w-tool" key={a.name}>
                <span className="bc-icon-tile">
                  <Icon name={a.icon} />
                </span>
                <span className="w-h3">{a.name}</span>
                <span className="w-tool__sub">{a.sub}</span>
                <span className="bc-muted">{a.body}</span>
                <a className="w-chipcmd" href={audienceHref(a.to)}>
                  <span>→</span>
                  <span>{a.link}</span>
                </a>
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* The six levels */}
      <section
        className="w-band"
        id="ladder"
        data-theme="light"
        style={{ paddingBottom: 128 }}
      >
        <div className="w-wrap">
          <div className="w-head" style={{ marginBottom: 40 }}>
            <div className="w-head__l">
              <h2 className="w-h2">{ladder.title}</h2>
            </div>
            <p className="w-lead" style={{ maxWidth: 380 }}>
              {ladder.lead}
            </p>
          </div>
          <Tour copy={copy} />
        </div>
      </section>

      {/* Chat apps */}
      <section className="w-band w-section" data-theme="light">
        <div className="w-wrap w-release">
          <div style={{ display: "flex", flexDirection: "column", gap: 24 }}>
            <h2 className="w-h2">{channels.title}</h2>
            <p className="w-lead">{channels.lead}</p>
            <div
              style={{
                display: "flex",
                flexWrap: "wrap",
                gap: 12,
                alignItems: "center",
              }}
            >
              <a className="bc-button w-btn-lg" href={RELEASES}>
                {channels.whatsNew}
              </a>
              <span className="bc-small bc-muted">{channels.installed}</span>
            </div>
          </div>
          <div>
            <div className="w-stats">
              {channels.stats.map(([k, v]) => (
                <div className="w-stat" key={k}>
                  <div className="w-stat__k">{k}</div>
                  <div className="w-stat__v">{v}</div>
                </div>
              ))}
            </div>
            <ul className="w-bullets">
              {channels.bullets.map(([b, rest]) => (
                <li key={b}>
                  <span>
                    <b>{b}</b>
                    {rest}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        </div>
      </section>

      {/* Plugins */}
      <section className="w-band w-section" id="plugins" data-theme="light">
        <div className="w-wrap w-center" style={{ gap: 20 }}>
          <h2 className="w-h2">{plugins.title}</h2>
          <p className="w-lead" style={{ maxWidth: 600 }}>
            {plugins.lead}
          </p>
          <div className="w-fam">
            {plugins.families.map((f) => (
              <a
                className="w-fam__cell"
                href={docs(lang, "plugins/overview")}
                key={f.title}
              >
                <span className="w-fam__top">
                  <span className="bc-icon-tile" aria-hidden="true">
                    <Icon name={f.icon} />
                  </span>
                </span>
                <span className="w-fam__title">{f.title}</span>
                <span className="bc-small bc-muted">{f.desc}</span>
              </a>
            ))}
            <a
              className="w-fam__cell w-fam__cell--all"
              href={docs(lang, "plugins/overview")}
            >
              <span className="w-fam__title">{plugins.allTitle}</span>
              <span className="bc-small bc-muted">{plugins.allBody}</span>
              <Icon name="arrowRight" className="w-fam__go" />
            </a>
          </div>
        </div>
      </section>

      {/* Board makers */}
      <section className="w-band w-section" id="makers" data-theme="light">
        <div className="w-wrap">
          <div className="w-head">
            <div className="w-head__l">
              <h2 className="w-h2">{makers.title}</h2>
            </div>
            <p className="w-lead" style={{ maxWidth: 400 }}>
              {makers.lead}
            </p>
          </div>
          <MakerPanel copy={copy} />
          <div
            className="w-head"
            style={{ margin: "96px 0 0", alignItems: "center" }}
          >
            <h3 className="w-h2" style={{ fontSize: 32, lineHeight: "38px" }}>
              {makers.supported}
            </h3>
            <a
              className="bc-button bc-button--outline"
              href={page(lang, "/boards/")}
            >
              {makers.seeAll}
            </a>
          </div>
          <div className="w-lib">
            {makers.lib.map((col) => (
              <div className="w-lib__col" key={col.group}>
                <span className="w-stat__k">{col.group}</span>
                {col.items.map(([name, desc]) => (
                  <div
                    key={name}
                    style={{ display: "flex", flexDirection: "column", gap: 2 }}
                  >
                    <span style={{ fontWeight: 500 }}>{name}</span>
                    <span className="bc-small bc-muted">{desc}</span>
                  </div>
                ))}
              </div>
            ))}
          </div>
        </div>
      </section>

      {/* Education */}
      <section className="w-band w-section" id="education" data-theme="light">
        <div className="w-wrap w-split">
          <div>
            <h2 className="w-h2" style={{ marginTop: 20 }}>
              {education.title}
            </h2>
            <p className="w-lead" style={{ marginTop: 20, maxWidth: 460 }}>
              {education.lead}
            </p>
            <div className="w-layers">
              {education.layers.map(([a, b], i) => (
                <div className="w-layer" key={a}>
                  <span
                    className={
                      i === education.layers.length - 1 ? "w-accent" : undefined
                    }
                  >
                    {a}
                  </span>
                  <span className="bc-muted">{b}</span>
                </div>
              ))}
            </div>
            <a
              className="bc-button w-btn-lg"
              href={docs(lang, "product/experience-ladder")}
              style={{ marginTop: 28 }}
            >
              {education.button}
            </a>
          </div>
          <img alt={education.alt} src="/images/board.svg" />
        </div>
      </section>

      {/* FAQ */}
      <section className="w-band w-section" data-theme="light">
        <div className="w-wrap">
          <div className="w-faqgrid">
            <div className="w-head__l">
              <h2 className="w-h2">{faq.title}</h2>
              <a className="bc-link" href={docs(lang)}>
                {faq.more}
              </a>
            </div>
            <Faq copy={copy} />
          </div>
        </div>
      </section>

      {/* Recent updates */}
      <section
        className="w-band w-updates"
        data-theme="light"
        style={{ paddingBottom: 128 }}
      >
        <div className="w-wrap">
          <h2 className="w-h2" style={{ fontSize: 32, lineHeight: "38px" }}>
            {updates.title}
          </h2>
          <div className="w-log">
            {updates.items.map(([date, text, pr]) => (
              <a
                href={`https://github.com/robcholz/barracuda/pull/${pr}`}
                key={pr}
              >
                <span className="bc-mono bc-caption bc-muted">{date}</span>
                <span style={{ fontSize: 16, fontWeight: 500 }}>{text}</span>
              </a>
            ))}
          </div>
          <a
            className="bc-link"
            href={RELEASES}
            style={{ display: "inline-block", marginTop: 20 }}
          >
            {updates.all}
          </a>
        </div>
      </section>

      {/* Closing */}
      <section
        className="w-band"
        data-theme="light"
        style={{ padding: "144px 0" }}
      >
        <div className="w-wrap w-final">
          <h2
            className="w-display"
            style={{ fontSize: 72, lineHeight: `${closing.titleLineHeight}px` }}
          >
            {closing.title}
          </h2>
          <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
            <p className="w-lead">{closing.lead}</p>
            <div className="bc-command">
              <span className="bc-command__prompt">$</span>
              <span className="bc-command__text">
                cargo board select &amp;&amp; cargo run
              </span>
            </div>
            <div style={{ display: "flex", flexWrap: "wrap", gap: 12 }}>
              <a className="bc-button w-btn-lg" href={install(lang)}>
                {closing.install}
              </a>
              <a
                className="bc-button bc-button--outline w-btn-lg"
                href={docs(lang)}
              >
                {closing.docs}
              </a>
            </div>
          </div>
        </div>
      </section>

      <Footer copy={copy} />
    </div>
  );
}
