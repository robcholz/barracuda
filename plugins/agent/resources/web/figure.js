// Barracuda figure "socket", from the approved design (live/src/socket.js). It uses the shell's global HL
// (the Hairline kernel) and never imports or bundles it.
/**
 * Socket: a board with four CPU sockets in a 2 × 2, one per Agent purpose, and
 * a chip above each. At rest the front chip (the root) is seated, bright, and
 * the other three hover at different heights over their sockets, each held by
 * two dashed drops. The pointer picks a socket: its chip comes down and seats,
 * the others lift back, staggered outwards from the one picked. The read-out
 * names the purpose. The slider is the stagger, in ms.
 *
 * The pattern: one of many, as Riffle. Tweens, a stagger by grid distance, and
 * a hit test on each cell's resting column, so a chip moving cannot flip it.
 */
const {
  Cam, extremes, facing, fit, poly, prism, proj, rings, rrect, ringAt, seg,
  tdone, tset, tval, tween, disposer, flatDot, mk, place, pointer, put, register, solid,
} = HL;

const CELL = 50, SK = 40, SH = 4, CT = 2.2, IT = 1.4, PAD = 7;
const NAMES = ["memory", "sub", "compact", "root"];
/** Cell order, back to front: [i, j]. The root is the nearest. */
const CELLS = [[0, 0], [1, 0], [0, 1], [1, 1]];
/** The chip's height over its socket's top: at rest, and when another chip is picked. */
const REST = [24, 14, 19, 0], AWAY = [20, 13, 16, 10];

function mount({ stage, svg, read }, value) {
  const bag = disposer();
  let stag = value;
  const E = 2 * CELL;
  const C = Cam(45, 0.5, 1.86);
  fit(C, [[-PAD, -PAD, -3], [E + PAD, E + PAD, -3], [E + PAD, -PAD, -3], [-PAD, E + PAD, -3], [0, 0, SH + 26 + CT + IT]], 200, 170);
  const P = proj(C), front = facing(C);
  const g = mk("g", {}, svg);

  const [br, bi] = rings(-PAD, -PAD, E + PAD, E + PAD, 8, 2);
  put(solid(g), prism(P, front, br, bi, -3, 0));

  const off = (CELL - SK) / 2;
  const cells = CELLS.map(([i, j], k) => {
    const x0 = i * CELL + off, y0 = j * CELL + off, x1 = x0 + SK, y1 = y0 + SK;
    const grp = mk("g", {}, g);
    const [sr, si] = rings(x0, y0, x1, y1, 3.5, 1.2);
    put(solid(grp), prism(P, front, sr, si, 0, SH));
    // the recess and its pads in a 5 × 5, lying on the socket: painted before the lever that stands on it
    mk("path", { d: poly(ringAt(P, rrect(x0 + 5, y0 + 5, x1 - 5, y1 - 5, 2, 4), SH)), class: "nf lo" }, grp);
    for (let p = 0; p < 25; p++) {
      const d = flatDot(grp, C, 0.55, "dot off");
      place(d, P(x0 + 10 + (p % 5) * 5, y0 + 10 + Math.floor(p / 5) * 5, SH));
    }
    // the retention lever along the right side, its handle turned in at the near end: all of it on the socket's top
    const [lr0, li0] = rings(x1 - 4.5, y0 + 4, x1 - 2.5, y1 - 2, 1, 0.4);
    put(solid(grp), prism(P, front, lr0, li0, SH, SH + 1.6));
    const [hr, hi] = rings(x1 - 4.5, y1 - 4, x1 - 1, y1 - 2, 1, 0.4);
    put(solid(grp), prism(P, front, hr, hi, SH, SH + 1.6));
    // the chip: its package, the lid on it, and the pin-1 mark
    const cx0 = x0 + 6.5, cy0 = y0 + 6.5, cx1 = x1 - 6.5, cy1 = y1 - 6.5;
    const [cr, ci] = rings(cx0, cy0, cx1, cy1, 2.5, 1);
    const [lr, li] = rings(cx0 + 5, cy0 + 5, cx1 - 5, cy1 - 5, 2, 0.8);
    const ext = extremes(P, cr).slice(0, 2);
    const guide = mk("path", { class: "nf dash" }, grp);
    const chip = solid(grp), lid = solid(grp);
    const pin = flatDot(grp, C, 0.7, "dot m");
    const mid = [(x0 + x1) / 2, (y0 + y1) / 2];
    return { k, i, j, cr, ci, lr, li, ext, guide, chip, lid, pin, cx0, cy0, mid, z: tween(SH + REST[k]), drawn: NaN };
  });

  function draw(c, z) {
    if (z === c.drawn) return;
    c.drawn = z;
    put(c.chip, prism(P, front, c.cr, c.ci, z, z + CT));
    put(c.lid, prism(P, front, c.lr, c.li, z + CT, z + CT + IT));
    place(c.pin, P(c.cx0 + 2.6, c.cy0 + 2.6, z + CT));
    c.guide.setAttribute("d", z - SH < 0.5 ? "" : c.ext.map((q) => seg(P(q.u, q.v, SH), P(q.u, q.v, z))).join(""));
  }

  const B = register(stage, (_dt, now) => {
    let moving = false;
    for (const c of cells) { draw(c, tval(c.z, now)); if (!tdone(c.z, now)) moving = true; }
    return moving;
  });
  bag.add(B.unregister);

  // hit columns: each cell's segment from its socket's top to its chip at rest. They never move.
  const cols = cells.map((c) => [P(c.mid[0], c.mid[1], SH), P(c.mid[0], c.mid[1], SH + REST[c.k] + CT)]);
  function hit([x, y]) {
    let best = -1, bd = 26;
    cols.forEach(([a, b], k) => {
      const dx = b[0] - a[0], dy = b[1] - a[1], L = dx * dx + dy * dy;
      const t = L ? Math.max(0, Math.min(1, ((x - a[0]) * dx + (y - a[1]) * dy) / L)) : 0;
      const d = Math.hypot(x - a[0] - t * dx, y - a[1] - t * dy);
      if (d < bd) { bd = d; best = k; }
    });
    return best;
  }

  let act = -1;
  function setActive(a) {
    if (a === act) return;
    const now = performance.now(), from = a >= 0 ? a : act >= 0 ? act : 3;
    act = a;
    for (const c of cells) {
      const f = cells[from], delay = (Math.abs(c.i - f.i) + Math.abs(c.j - f.j)) * stag;
      const h = a < 0 ? REST[c.k] : c.k === a ? 0 : AWAY[c.k];
      tset(c.z, SH + h, now, delay);
      c.chip.sil.classList.toggle("hi", a < 0 ? c.k === 3 : c.k === a);
      c.pin.classList.toggle("m", !(a < 0 ? c.k === 3 : c.k === a));
    }
    read.textContent = a < 0 ? "rest" : NAMES[a];
    B.wake();
  }
  cells[3].chip.sil.classList.add("hi");
  cells[3].pin.classList.remove("m");

  bag.add(pointer(stage, { move: (p) => setActive(hit(p)), leave: () => setActive(-1) }));
  bag.add(() => svg.replaceChildren());

  return {
    set: (v) => { stag = v; },
    destroy: bag.dispose,
  };
}

export const figure = {
  name: "socket",
  means: "Four sockets, one per Agent purpose: the chip over the one under the pointer comes down and seats.",
  rules: [1, 2, 5, 6],
  range: [0, 50, 110],
  mount,
};
