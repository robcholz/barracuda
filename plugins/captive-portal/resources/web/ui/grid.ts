/**
 * A `grid-template-columns` value for `count` equal columns that reflows on narrow screens: exactly
 * `count` columns while each can be at least `min` px wide, fewer below that, and one full-width
 * column on a phone. No media query, so it works in an inline style.
 */
export function fitColumns(count: number, min: number, gap: string): string {
  const share =
    count > 1 ? `calc((100% - ${count - 1} * ${gap}) / ${count})` : "100%";
  return `repeat(auto-fill,minmax(max(min(100%,${min}px),${share}),1fr))`;
}
