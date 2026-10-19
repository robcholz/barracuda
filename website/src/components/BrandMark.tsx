import { MARK_ART, MARK_VIEWBOX } from "../../../design-system/marks";

/** The Barracuda mark: the lime tile and its ink outline (design-system/marks.ts). */
export function BrandMark() {
  return (
    <svg
      viewBox={MARK_VIEWBOX}
      width="24"
      height="24"
      aria-hidden="true"
      style={{ flex: "none" }}
      dangerouslySetInnerHTML={{ __html: MARK_ART }}
    />
  );
}
