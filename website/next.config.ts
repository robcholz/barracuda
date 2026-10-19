import type { NextConfig } from "next";

// A static site: every page is prerendered, so Vercel serves files only.
const config: NextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
  // `next dev` would otherwise write an AGENTS.md here; the repository keeps its own at the root.
  agentRules: false,
};

export default config;
