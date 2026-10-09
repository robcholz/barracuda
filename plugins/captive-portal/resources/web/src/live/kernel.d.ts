/** The Hairline kernel (`kernel.js`), typed only as far as the shell uses it. Figures are plain JS. */
export interface HairlineKernel {
  inject(root: Document | ShadowRoot): void;
  mk(
    tag: string,
    attrs?: Record<string, string | number>,
    parent?: Element,
  ): SVGElement;
  reducedMotion(): boolean;
  [name: string]: unknown;
}
declare const HL: HairlineKernel;
export default HL;
