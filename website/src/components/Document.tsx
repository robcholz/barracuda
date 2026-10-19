import type { ReactNode } from "react";
import "@/styles/ds/tokens.css";
import "@/styles/ds/bundle.css";
import "@/styles/site.css";

/** The page shell shared by both languages: one root layout per language sets `lang`. */
export function Document({
  lang,
  children,
}: {
  lang: string;
  children: ReactNode;
}) {
  return (
    <html lang={lang}>
      <body>{children}</body>
    </html>
  );
}
