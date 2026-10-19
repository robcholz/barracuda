import type { Copy } from "@/content/en";
import { GITHUB, ISSUES, RELEASES, docs, install, page } from "@/lib/links";

/** The footer: links on a frosted strip over the painting, then the wordmark. */
export function Footer({ copy }: { copy: Copy }) {
  const { footer: f, lang } = copy;
  const home = page(lang, "/");
  return (
    <footer
      className="w-band w-foot-band"
      data-theme="light"
      style={{ overflow: "hidden" }}
    >
      <div className="w-wrap">
        <div className="w-foot">
          <div className="w-foot__col">
            <span style={{ fontWeight: 600, fontSize: 16 }}>Barracuda</span>
            <span style={{ maxWidth: 240 }}>{f.tagline}</span>
          </div>
          <div className="w-foot__col">
            <span className="w-foot__h">{f.product}</span>
            <a href={install(lang)}>{f.install}</a>
            <a href={page(lang, "/boards/")}>{f.boards}</a>
            <a href={`${home}#plugins`}>{f.plugins}</a>
            <a href={RELEASES}>{f.changelog}</a>
          </div>
          <div className="w-foot__col">
            <span className="w-foot__h">{f.developers}</span>
            <a href={docs(lang)}>{f.docs}</a>
            <a href={docs(lang, "develop/write-a-plugin")}>{f.writePlugin}</a>
            <a href={GITHUB}>GitHub</a>
          </div>
          <div className="w-foot__col">
            <span className="w-foot__h">{f.partners}</span>
            <a href={`${home}#makers`}>{f.boardMakers}</a>
            <a href={`${home}#education`}>{f.education}</a>
          </div>
          <div className="w-foot__col">
            <span className="w-foot__h">{f.community}</span>
            <a href={ISSUES}>{f.issue}</a>
          </div>
        </div>
        <p className="w-word" aria-hidden="true">
          Barracuda
        </p>
      </div>
    </footer>
  );
}
