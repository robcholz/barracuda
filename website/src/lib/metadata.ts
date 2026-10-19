import type { Metadata } from "next";
import type { Copy } from "@/content/en";
import { page } from "./links";

/** Title, description and the other language's address for one page. */
export function metadata(
  copy: Copy,
  which: "home" | "boards" | "install",
): Metadata {
  const path = ({ home: "/", boards: "/boards/", install: "/install/" } as const)[
    which
  ];
  const { meta } = copy;
  return {
    title: { home: meta.title, boards: meta.boardsTitle, install: meta.installTitle }[
      which
    ],
    description: {
      home: meta.description,
      boards: meta.boardsDescription,
      install: meta.installDescription,
    }[which],
    icons: { icon: "/favicon.svg" },
    alternates: {
      canonical: page(copy.lang, path),
      languages: { en: page("en", path), "zh-Hans": page("zh", path) },
    },
  };
}
