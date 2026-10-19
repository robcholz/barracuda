import { Installer } from "@/components/Installer";
import { zh } from "@/content/zh";
import { metadata as meta } from "@/lib/metadata";
import "@/styles/install.css";

export const metadata = meta(zh, "install");

export default function Page() {
  return <Installer lang="zh" />;
}
