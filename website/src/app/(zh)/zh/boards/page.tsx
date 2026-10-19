import { Boards } from "@/components/Boards";
import { Footer } from "@/components/Footer";
import { zh } from "@/content/zh";
import { metadata as meta } from "@/lib/metadata";

export const metadata = meta(zh, "boards");

export default function Page() {
  return <Boards copy={zh} footer={<Footer copy={zh} />} />;
}
