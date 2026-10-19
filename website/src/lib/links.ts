export type Lang = "en" | "zh";

export const GITHUB = "https://github.com/robcholz/barracuda";
export const RELEASES = `${GITHUB}/releases`;
export const ISSUES = `${GITHUB}/issues`;

const DOCS = "https://barracuda-os.mintlify.site";

/** A docs page in the reader's language: `docs("zh", "use/scripts")`. */
export function docs(lang: Lang, path = ""): string {
  const prefix = lang === "zh" ? "/zh" : "";
  return `${DOCS}${prefix}${path ? `/${path}` : prefix ? "" : "/"}`;
}

/** Live Editor, which this site's build serves (docs/tools/live-editor.mdx, Deploy). */
export const EDITOR = "/editor/";

/** Where every install button goes: the browser installer. */
export function install(lang: Lang): string {
  return page(lang, "/install/");
}

/** A page of this site in the reader's language. */
export function page(
  lang: Lang,
  path: "/" | "/boards/" | "/install/",
): string {
  return lang === "zh" ? `/zh${path}` : path;
}
