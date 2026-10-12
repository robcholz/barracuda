// Barracuda figure "cabinet". It uses the shell's global HL (the Hairline kernel) and never imports or bundles it.
/**
 * Cabinet: a filing cabinet of four drawers, each a place the files are kept.
 * At rest the second drawer from the top stands part way out, two files
 * showing over its rim and its pull bright. The pointer picks a drawer by
 * where it falls on the drawer fronts; that drawer slides out on its files and
 * the others close, staggered outwards from it. Each drawer's number is a row
 * of dots in its label holder. The slider is how far a drawer comes out.
 *
 * The pattern: one of many, so tweens and a stagger by distance; the pick is
 * made on the fronts' fixed planes, at rest or at the open drawer's target.
 */
const {
  Cam,
  clamp,
  facing,
  fillet,
  fit,
  hull,
  open,
  poly,
  proj,
  prism,
  put,
  ringAt,
  rings,
  rrect,
  run,
  tdone,
  tset,
  tval,
  tween,
  disposer,
  mk,
  place,
  pointer,
  register,
  solid,
} = HL;

const D = 58,
  W = 56,
  N = 4,
  HD = 18,
  GAP = 2,
  BASE = 4,
  LID = 3,
  T = 2.4; // body depth and width, drawers, plinth, lid, panel
const TOP = BASE + N * HD + (N + 1) * GAP;
const RIM = HD - 6,
  PITCH = 6,
  MAXF = 7,
  TABS = [8, 22, 36],
  TW = 11; // the box's rim, the files' pitch, their tabs
const REST_I = 2,
  REST_E = 15,
  STEP = 45,
  FAR = 44,
  S = 2.15;
const z0 = (i) => BASE + GAP + i * (HD + GAP);

/** A file standing in a drawer, above the rim: its outline in its own (y, z) plane, tab at one of three places. */
const file = (t0) =>
  fillet(
    [
      [6, 0],
      [W - 6, 0],
      [W - 6, 3],
      [t0 + TW, 3],
      [t0 + TW, 5.5],
      [t0, 5.5],
      [t0, 3],
      [6, 3],
    ],
    [0.5, 0.5, 1, 1.2, 1.5, 1.5, 1.2, 1],
  );

function mount({ stage, svg, read }, value) {
  const bag = disposer();
  let reach = value;

  // fitted with the top and bottom drawers at the slider's far end, so no pose leaves the frame
  const C = Cam(45, 0.5, S);
  const ext = [
    [-1.5, -1.5, TOP + LID],
    [D + 1.5, -1.5, TOP + LID],
    [-1.5, W + 1.5, TOP + LID],
    [D, W, 0],
    [0, W, 0],
  ];
  for (const i of [0, N - 1])
    for (const y of [2, W - 2])
      for (const z of [z0(i), z0(i) + HD]) ext.push([D + FAR + T, y, z]);
  fit(C, ext, 200, 166);
  const P = proj(C),
    front = facing(C);
  const zf = Math.sqrt(1 - C.k * C.k),
    ca = Math.cos(C.az),
    sa = Math.sin(C.az);
  /** A sample of a ring drawn in a drawer front's (y, z) plane that faces the camera. */
  const vis = (q) => q.nu * ca * zf + q.nv * C.k > 0;

  const g = mk("g", {}, svg);
  // the cabinet, back to front: a recessed plinth, the body, a lid that overhangs it
  const [po, pi] = rings(3, 3, D - 3, W - 3, 4, 1.2);
  put(solid(g), prism(P, front, po, pi, 0, BASE));
  const [bo] = rings(0, 0, D, W, 4, 1.4);
  put(solid(g), prism(P, front, bo, null, BASE, TOP));
  const [lo, li] = rings(-1.5, -1.5, D + 1.5, W + 1.5, 5, 1.6);
  put(solid(g), prism(P, front, lo, li, TOP, TOP + LID));

  // a drawer front: the panel, its bevel, a finger pull, a label holder holding the drawer's number in dots
  const panel = rrect(2, 0, W - 2, HD, 2.5, 4),
    bevel = rrect(3.2, 1.2, W - 3.2, HD - 1.2, 1.3, 4);
  const pull = rrect(W / 2 - 10, 4.5, W / 2 + 10, 9, 2.25, 4),
    pullIn = rrect(W / 2 - 8.4, 5.7, W / 2 + 8.4, 7.8, 1, 4);
  const holder = rrect(W / 2 - 6.5, 11, W / 2 + 6.5, 14.8, 1, 3);
  const shapes = TABS.map(file);

  // bottom to top: a drawer coming out covers the ones under it
  const drawers = [];
  for (let i = 0; i < N; i++) {
    const grp = mk("g", {}, g),
      n = N - i;
    const box = mk("path", { class: "sil" }, grp),
      rim = mk("path", { class: "nf" }, grp);
    const files = Array.from({ length: MAXF }, () => mk("path", {}, grp));
    const pnl = solid(grp);
    const pl = mk("path", { class: "nf" }, grp),
      pin = mk("path", { class: "nf lo" }, grp),
      hold = mk("path", { class: "nf lo" }, grp);
    const dots = Array.from({ length: n }, () =>
      mk("circle", { r: 0.95, class: "dot off" }, grp),
    );
    drawers.push({
      i,
      n,
      box,
      rim,
      files,
      pnl,
      pl,
      pin,
      hold,
      dots,
      e: tween(i === REST_I ? REST_E : 0),
      last: NaN,
    });
  }

  function draw(d, e) {
    if (e === d.last) return;
    d.last = e;
    const zb = z0(d.i),
      xf = D + e;
    if (e > 4) {
      // the box, from just inside the body to the panel, open at the top
      const [o, inn] = rings(D - 1, 4, xf, W - 4, 2, 1.2);
      d.box.setAttribute(
        "d",
        poly(hull(ringAt(P, o, zb + 1).concat(ringAt(P, o, zb + RIM)))),
      );
      d.rim.setAttribute("d", poly(ringAt(P, inn, zb + RIM)));
    } else {
      d.box.setAttribute("d", "");
      d.rim.setAttribute("d", "");
    }
    // the files it carries, counted from its front so each keeps its tab, drawn far to near; the body hides the rest
    const xs = [];
    for (let m = 0; m < MAXF && xf - 4 - m * PITCH > D + 1.5; m++)
      xs.unshift([xf - 4 - m * PITCH, m]);
    d.files.forEach((el, k) => {
      const f = xs[k];
      el.setAttribute(
        "d",
        f
          ? poly(
              shapes[(f[1] + d.i) % 3].map((p) =>
                P(f[0], p[0], zb + RIM + p[1]),
              ),
            )
          : "",
      );
    });
    const at = (q, x) => P(x, q.u, zb + q.v),
      face = xf + T;
    put(d.pnl, {
      sil: poly(
        hull(panel.map((q) => at(q, xf)).concat(panel.map((q) => at(q, face)))),
      ),
      crease: open(run(bevel, vis).map((q) => at(q, face))),
    });
    d.pl.setAttribute("d", poly(pull.map((q) => at(q, face))));
    d.pin.setAttribute("d", poly(pullIn.map((q) => at(q, face))));
    d.hold.setAttribute("d", poly(holder.map((q) => at(q, face))));
    d.dots.forEach((el, k) =>
      place(el, at({ u: W / 2 - (d.n - 1) * 1.5 + k * 3, v: 12.9 }, face)),
    );
  }

  const B = register(stage, (_dt, now) => {
    let moving = false;
    for (const d of drawers) {
      draw(d, tval(d.e, now));
      if (!tdone(d.e, now)) moving = true;
    }
    return moving;
  });
  bag.add(B.unregister);

  let act = -1;
  /** Where a screen point falls on the plane of the drawer fronts standing x out from the body. */
  const onFront = ([sx, sy], x) => {
    const y = (x * ca - (sx - C.ox) / C.S) / sa;
    return [y, ((x * sa + y * ca) * C.k - (sy - C.oy) / C.S) / zf];
  };
  /** Whether a screen point lies inside the outline drawer i covers when it stands e out. */
  function covers(i, e, [x, y]) {
    const pts = [];
    for (const px of [D, D + e + T])
      for (const py of [2, W - 2])
        for (const pz of [z0(i), z0(i) + HD]) pts.push(P(px, py, pz));
    const h = hull(pts);
    return h.every((a, k) => {
      const b = h[(k + 1) % h.length];
      return (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]) >= 0;
    });
  }
  /** The drawer under the point: the drawer that is out is tested at its target, the rest at their fronts' fixed plane. */
  function hit(p) {
    const o = act >= 0 ? act : REST_I;
    if (covers(o, act >= 0 ? reach : REST_E, p)) return o;
    const [y, z] = onFront(p, D + T);
    if (y < -2 || y > W + 2 || z < BASE || z > TOP) return -1;
    return clamp(Math.floor((z - BASE - GAP / 2) / (HD + GAP)), 0, N - 1);
  }
  /** One bright thing: the pull, and the number in dots, of the drawer that is out. */
  function light(lit) {
    for (const d of drawers) {
      d.pl.classList.toggle("hi", d.i === lit);
      for (const el of d.dots)
        el.setAttribute("class", d.i === lit ? "dot m" : "dot off");
    }
  }
  function setActive(a) {
    if (a === act) return;
    const now = performance.now(),
      from = a >= 0 ? a : act;
    act = a;
    for (const d of drawers) {
      const to = a < 0 ? (d.i === REST_I ? REST_E : 0) : d.i === a ? reach : 0;
      tset(d.e, to, now, Math.abs(d.i - from) * STEP);
    }
    light(a < 0 ? REST_I : a);
    read.textContent = a < 0 ? "rest" : String(N - a).padStart(2, "0");
    B.wake();
  }
  light(REST_I);
  read.textContent = "rest";

  bag.add(
    pointer(stage, {
      move: (p) => setActive(hit(p)),
      leave: () => setActive(-1),
    }),
  );
  bag.add(() => svg.replaceChildren());

  return {
    set: (v) => {
      reach = v;
      if (act >= 0) tset(drawers[act].e, reach, performance.now(), 0);
      B.wake();
    },
    destroy: bag.dispose,
  };
}

export const figure = {
  name: "cabinet",
  means:
    "A filing cabinet: the drawer under the pointer slides out on its files, and the others close in turn.",
  rules: [1, 2, 5, 6],
  range: [24, 34, 44],
  view: [18.9, 23.6, 322.6, 258.1],
  answer: [216, 160],
  mount,
};
