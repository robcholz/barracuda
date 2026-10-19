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

/** Where every install button goes until the browser installer ships. */
export function install(lang: Lang): string {
  return docs(lang, "get-started/installation");
}

/** A page of this site in the reader's language. */
export function page(lang: Lang, path: "/" | "/boards/"): string {
  return lang === "zh" ? `/zh${path}` : path;
}
