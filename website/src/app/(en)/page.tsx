import { Home } from "@/components/Home";
import { en } from "@/content/en";
import { metadata as meta } from "@/lib/metadata";

export const metadata = meta(en, "home");

export default function Page() {
  return <Home copy={en} />;
}
