import { Home } from "@/components/Home";
import { zh } from "@/content/zh";
import { metadata as meta } from "@/lib/metadata";

export const metadata = meta(zh, "home");

export default function Page() {
  return <Home copy={zh} />;
}
