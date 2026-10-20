import type { ReactNode } from "react";
import { Analytics } from "@vercel/analytics/next";
import { SpeedInsights } from "@vercel/speed-insights/next";
import "../../../ui/tokens.css";
import "../../../ui/components.css";
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
      <body>
        {children}
        <Analytics />
        <SpeedInsights />
      </body>
    </html>
  );
}
