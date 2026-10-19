import type { Metadata } from "next";
import type { Copy } from "@/content/en";
import { page } from "./links";

/** Title, description and the other language's address for one page. */
export function metadata(copy: Copy, which: "home" | "boards"): Metadata {
  const path = which === "home" ? "/" : "/boards/";
  return {
    title: which === "home" ? copy.meta.title : copy.meta.boardsTitle,
    description:
      which === "home" ? copy.meta.description : copy.meta.boardsDescription,
    icons: { icon: "/favicon.svg" },
    alternates: {
      canonical: page(copy.lang, path),
      languages: { en: page("en", path), "zh-Hans": page("zh", path) },
    },
  };
}
