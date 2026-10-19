import { Installer } from "@/components/Installer";
import { en } from "@/content/en";
import { metadata as meta } from "@/lib/metadata";
import "@/styles/install.css";

export const metadata = meta(en, "install");

export default function Page() {
  return <Installer lang="en" />;
}
