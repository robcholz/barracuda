import { Boards } from "@/components/Boards";
import { Footer } from "@/components/Footer";
import { en } from "@/content/en";
import { metadata as meta } from "@/lib/metadata";

export const metadata = meta(en, "boards");

export default function Page() {
  return <Boards copy={en} footer={<Footer copy={en} />} />;
}
