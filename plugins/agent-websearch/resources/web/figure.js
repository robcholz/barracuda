// Barracuda figure "loupe", from the approved design (live/src/loupe.js). It uses the shell's global HL
// (the Hairline kernel) and never imports or bundles it.
/**
 * Loupe: a stand loupe on three legs over a ruled sheet with nothing written
 * on it, after the package's Loupe. At rest the glass sits up and to the left
 * on the sheet, centred on the third rule and just clear of the margin; the
 * pointer coming onto the figure slides it down that rule to the middle of the
 * sheet, and leaving slides it back. Under the glass the rules are drawn
 * again, larger, about the point the eye sees through the glass's centre, so
 * the rule through that point runs straight on through the glass and the
 * others open out evenly on either side. Its thin rim is the bright mark. The
 * slider is the magnification.
 *
 * The pattern: one discrete move, so a tween (rule 08), with all three feet
 * on the sheet at both ends of it.
 */
const {
  Cam, circ, clamp, facing, fit, hull, open, poly, prism, proj, rings, rrect, seg, unproj,
  spring, stepS, tdone, tset, tval, tween, disposer, mk, pointer, put, register, solid,
} = HL;

const SW = 124, SD = 92, ST = 2.4;                  // the sheet: width, depth, thickness
const ROWS = 7, Y0 = 13, DY = 11, ML = 26, MR = 12; // its rules: how many, the first one's y, their pitch, the left margin, the right one
const R = 27, LZ = 20, RIM = 1.2, GLASS = R - 1.4;  // the loupe: ring radius, height of its underside, ring depth, glass radius
const LEGS = [45, 165, 285], FOOT = 35, PAD = 1.8, S = 2.1;
const REST = [46, Y0 + 2 * DY];                     // at rest: up and left on the sheet, on the third rule, just clear of the margin
const MID = 64;                                     // on the pointer: slid down the rule to the middle of the sheet, feet still on it
const rowY = (i) => Y0 + i * DY;

function mount({ stage, svg, read }, value) {
  const bag = disposer();
  const C = Cam(45, 0.5, S), top = LZ + RIM;
  const feet = LEGS.map((a) => [Math.cos((a * Math.PI) / 180), Math.sin((a * Math.PI) / 180)]);
  /** From the point the eye sees to the loupe's centre: the glass stands this far in front of it. */
  const [bx, by] = unproj(C, ...proj(C)(0, 0, 0), top);
  const ext = [[0, 0, -ST], [SW, 0, -ST], [0, SD, -ST], [SW, SD, -ST]];
  for (const q of circ(R, 16)) ext.push([REST[0] + bx + q.u, REST[1] + by + q.v, top], [MID + bx + q.u, REST[1] + by + q.v, top]);
  fit(C, ext, 200, 166);
  const P = proj(C), front = facing(C);
  const g = mk("g", {}, svg);

  // The sheet, then its rules lying on it.
  const [so, si] = rings(0, 0, SW, SD, 6, 1.4);
  put(solid(g), prism(P, front, so, si, -ST, 0));
  const lines = [[[ML, Y0 - 7], [ML, rowY(ROWS - 1) + 7]], ...Array.from({ length: ROWS }, (_, i) => [[ML, rowY(i)], [SW - MR, rowY(i)]])];
  mk("path", { class: "nf lo", d: lines.map(([p, q]) => seg(P(p[0], p[1], 0), P(q[0], q[1], 0))).join("") }, g);

  // The loupe: three legs, a thin ring round the glass, a glint on it, the sheet seen through it.
  const legs = mk("path", { class: "nf" }, g);
  const rim = solid(g);
  const glint = mk("path", { class: "nf lo" }, g);
  const seen = mk("path", { class: "nf sil" }, g);
  const shine = circ(GLASS * 0.72, 64).slice(26, 36);
  const ring = circ(R, 64), lens = circ(GLASS, 64), edge = rrect(0, 0, SW, SD, 6, 12), pad = circ(PAD, 12);
  /** A foot on the sheet stands on its top; one past its edge, on the table under it. */
  const floor = (x, y) => (x >= 0 && x <= SW && y >= 0 && y <= SD ? 0 : -ST);

  // where the glass is: one discrete move, rest to the middle and back, so a 700ms tween (rule 08)
  const tx = tween(REST[0]), sy = { x: REST[1] }, mag = spring(value);
  let gx = REST[0];
  let cx = NaN, cy = NaN, dm = NaN;
  function draw() {
    if (gx === cx && sy.x === cy && mag.x === dm) return;
    const moved = gx !== cx || sy.x !== cy;
    cx = gx; cy = sy.x; dm = mag.x;
    const dx = cx + bx, dy = cy + by;
    if (moved) {
      const at = (q, z) => P(dx + q.u, dy + q.v, z);
      legs.setAttribute("d", feet.map(([c, s]) => {
        const fx = dx + c * FOOT, fy = dy + s * FOOT, z = floor(fx, fy);
        return seg(P(fx, fy, z), P(dx + c * (R - 1), dy + s * (R - 1), LZ)) + poly(pad.map((q) => P(fx + q.u, fy + q.v, z)));
      }).join(""));
      put(rim, { sil: poly(hull(ring.map((q) => at(q, LZ)).concat(ring.map((q) => at(q, top))))), crease: poly(lens.map((q) => at(q, top))) });
      glint.setAttribute("d", open(shine.map((q) => at(q, top))));
    }
    // A line on the sheet, enlarged about the point the eye sees through the glass's centre, kept where it falls inside the glass.
    const r = GLASS - 1;
    const cut = (p, q) => {
      const a0 = (p[0] - cx) * dm, a1 = (p[1] - cy) * dm, e0 = (q[0] - p[0]) * dm, e1 = (q[1] - p[1]) * dm;
      const A = e0 * e0 + e1 * e1, B = a0 * e0 + a1 * e1, D = B * B - A * (a0 * a0 + a1 * a1 - r * r);
      if (D <= 0) return "";
      const t0 = Math.max(0, (-B - Math.sqrt(D)) / A), t1 = Math.min(1, (-B + Math.sqrt(D)) / A);
      return t1 > t0 ? seg(P(dx + a0 + e0 * t0, dy + a1 + e1 * t0, top), P(dx + a0 + e0 * t1, dy + a1 + e1 * t1, top)) : "";
    };
    let d = edge.map((q, i) => cut([q.u, q.v], [edge[(i + 1) % edge.length].u, edge[(i + 1) % edge.length].v])).join("");
    for (const [p, q] of lines) d += cut(p, q);
    seen.setAttribute("d", d);
  }

  const loop = register(stage, (dt, now) => { gx = tval(tx, now); const c = stepS(mag, dt); draw(); return !tdone(tx, now) || c; });
  bag.add(loop.unregister);
  rim.sil.classList.add("hi");
  read.textContent = "rest";
  draw();

  // The pointer coming onto the figure slides the glass down its rule to the middle of the sheet; leaving slides it back.
  let on = false;
  const slide = (to) => { on = to; tset(tx, to ? MID : REST[0], performance.now(), 0); read.textContent = to ? "row 3 · 0" : "rest"; loop.wake(); };
  bag.add(pointer(stage, { move: () => { if (!on) slide(true); }, leave: () => slide(false) }));
  bag.add(() => svg.replaceChildren());

  return { set: (v) => { mag.t = v; loop.wake(); }, destroy: bag.dispose };
}

export const figure = {
  name: "loupe",
  means: "A stand loupe over a ruled sheet: the rule under the glass runs straight through it, and its neighbours open out.",
  rules: [1, 5, 6, 9],
  range: [1.3, 1.5, 1.8],
  mount,
};
