import type { NextConfig } from "next";

// A static site: every page is prerendered, so Vercel serves files only.
const config: NextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
};

export default config;
