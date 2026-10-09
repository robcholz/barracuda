// Barracuda figure "laptop", from the approved design (live/src/laptop.js). It uses the shell's global HL
// (the Hairline kernel) and never imports or bundles it.
// Ported from @lucasmarkes/hairline (MIT) src/figures/laptop.ts for the Barracuda portal: types stripped.
const {
  Cam,
  clamp,
  disposer,
  facing,
  fit,
  hull,
  mk,
  open,
  place,
  pointer,
  poly,
  prism,
  proj,
  put,
  rad,
  register,
  rings,
  rrect,
  run,
  seg,
  solid,
  tdone,
  tset,
  tval,
  tween,
} = HL;
const W = 150,
  D = 106,
  HB = 4,
  T = 2.4,
  HZ = HB + T / 2,
  R = 7,
  B = 1.1;
const MIN = 15,
  REST = 100,
  BZ = 4.2,
  CHIN = 7.5,
  KU = 8.6,
  KY = 6;
const lidAt = (th) => {
  const c = Math.cos(rad(th)),
    s = Math.sin(rad(th));
  return (u, v, w) => [u, v * c - w * s, HZ + v * s + w * c];
};
function keys() {
  const out = [],
    x0 = (W - 14.5 * KU) / 2;
  const row = (y, h, widths) => {
    let x = x0;
    for (const w of widths) {
      out.push([x, y, x + w * KU, y + h]);
      x += w * KU;
    }
  };
  const ones = (n) => Array(n).fill(1);
  row(KY, KU * 0.6, Array(14).fill(14.5 / 14));
  let y = KY + KU * 0.6;
  for (const w of [
    [...ones(13), 1.5],
    [1.5, ...ones(13)],
    [1.75, ...ones(11), 1.75],
    [2.25, ...ones(10), 2.25],
  ]) {
    row(y, KU, w);
    y += KU;
  }
  row(y, KU, [1, 1, 1, 1.25, 5, 1.25, 1]);
  const ax = x0 + 11.5 * KU,
    h = KU / 2;
  out.push(
    [ax, y + h, ax + KU, y + KU],
    [ax + KU, y, ax + 2 * KU, y + h],
    [ax + KU, y + h, ax + 2 * KU, y + KU],
    [ax + 2 * KU, y + h, ax + 3 * KU, y + KU],
  );
  return { out, y1: y + KU, x0 };
}
const mount = ({ stage, svg, read }, value) => {
  const bag = disposer();
  let maxA = value,
    over = false;
  const C = Cam(45, 0.5, 1.3),
    pts = [
      [0, 0, 0],
      [W, 0, 0],
      [0, D, 0],
      [W, D, 0],
    ];
  for (const th of [MIN, 90, REST, 125])
    for (const u of [0, W])
      for (const v of [0, D])
        for (const w of [-T / 2, T / 2]) pts.push(lidAt(th)(u, v, w));
  fit(C, pts, 200, 166);
  const P = proj(C),
    front = facing(C);
  const o = P(0, 0, 0),
    ex = P(1, 0, 0),
    ey = P(0, 1, 0),
    ez = P(0, 0, 1);
  const r1 = [ex[0] - o[0], ey[0] - o[0], ez[0] - o[0]],
    r2 = [ex[1] - o[1], ey[1] - o[1], ez[1] - o[1]];
  let eye = [
    r1[1] * r2[2] - r1[2] * r2[1],
    r1[2] * r2[0] - r1[0] * r2[2],
    r1[0] * r2[1] - r1[1] * r2[0],
  ];
  if (eye[2] < 0) eye = eye.map((n) => -n);
  const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const g = mk("g", {}, svg);
  const [br, bi] = rings(0, 0, W, D, R, B);
  put(solid(g), prism(P, front, br, bi, 0, HB));
  const top = (q) => poly(q.map((s) => P(s.u, s.v, HB)));
  const kb = keys();
  mk(
    "path",
    {
      class: "nf lo",
      d: kb.out
        .map(([a, b, c, d]) =>
          top(rrect(a + 0.5, b + 0.5, c - 0.5, d - 0.5, 1.1, 2)),
        )
        .join(""),
    },
    g,
  );
  mk(
    "path",
    {
      class: "nf",
      d: top(rrect(kb.x0 - 1.6, KY - 1.6, W - kb.x0 + 1.6, kb.y1 + 1.6, 3, 4)),
    },
    g,
  );
  mk(
    "path",
    {
      class: "nf",
      d: top(rrect(W / 2 - 35, D - 45, W / 2 + 35, D - 5, 3.2, 4)),
    },
    g,
  );
  const lidG = mk("g", {}, g),
    lid = solid(lidG),
    scr = mk("g", {}, lidG);
  lid.sil.classList.add("hi");
  const [lr, li] = rings(0, 0, W, D, R, B);
  const disp = mk("path", { class: "nf" }, scr),
    bar = mk("path", { class: "nf lo" }, scr),
    dock = mk("path", { class: "nf lo" }, scr);
  const win = mk("path", {}, scr),
    strip = mk("path", { class: "nf lo" }, scr);
  const cam = mk("circle", { r: 0.8, class: "dot off" }, scr);
  const lights = [0, 1, 2].map(() =>
    mk("circle", { r: 0.8, class: "dot off" }, scr),
  );
  const VT = D - BZ,
    WX0 = 30,
    WX1 = 104,
    WV0 = CHIN + 16,
    WV1 = VT - 12;
  let faced = null,
    drawn = NaN;
  function drawLid(th) {
    if (th === drawn) return;
    drawn = th;
    const f = lidAt(th),
      c = Math.cos(rad(th)),
      s = Math.sin(rad(th));
    const at = (u, v, w) => P(...f(u, v, w)),
      on = (u, v) => at(u, v, -T / 2);
    const plane = (q, w) => q.map((p) => at(p.u, p.v, w));
    const shows = dot([0, s, -c], eye) > 0,
      wf = shows ? -T / 2 : T / 2;
    const wall = (q) => dot([q.nu, q.nv * c, q.nv * s], eye) > 0;
    put(lid, {
      sil: poly(hull(plane(lr, -T / 2).concat(plane(lr, T / 2)))),
      crease: open(plane(run(li, wall), wf)),
    });
    if (shows !== faced) {
      faced = shows;
      if (shows) lid.g.after(scr);
      else lid.g.before(scr);
    }
    const face = (q) => poly(q.map((p) => on(p.u, p.v)));
    disp.setAttribute("d", face(rrect(BZ, CHIN, W - BZ, VT, 3, 4)));
    bar.setAttribute("d", seg(on(BZ + 1.5, VT - 4), on(W - BZ - 1.5, VT - 4)));
    dock.setAttribute(
      "d",
      face(rrect(W / 2 - 28, CHIN + 2.5, W / 2 + 28, CHIN + 8.5, 2.2, 4)),
    );
    win.setAttribute("d", face(rrect(WX0, WV0, WX1, WV1, 2.6, 4)));
    strip.setAttribute(
      "d",
      seg(on(WX0, WV1 - 6), on(WX1, WV1 - 6)) +
        seg(on(WX0 + 20, WV0), on(WX0 + 20, WV1 - 6)),
    );
    place(cam, on(W / 2, D - BZ / 2));
    lights.forEach((el, k) => place(el, on(WX0 + 4 + k * 3.4, WV1 - 3)));
  }
  // The pointer coming onto the laptop opens the lid a little further, and leaving lets it back: one discrete
  // change, so a 700ms tween (rule 08), not a spring chasing the pointer.
  const tw = tween(REST);
  const Bk = register(stage, (_dt, now) => {
    drawLid(tval(tw, now));
    return !tdone(tw, now);
  });
  bag.add(Bk.unregister);
  drawLid(REST);
  function retarget() {
    tset(tw, over ? maxA : Math.min(REST, maxA), performance.now(), 0);
    read.textContent = over ? `lid ${Math.round(maxA)}°` : "rest";
    Bk.wake();
  }
  bag.add(
    pointer(stage, {
      move: () => {
        if (over) return;
        over = true;
        retarget();
      },
      leave: () => {
        over = false;
        retarget();
      },
    }),
  );
  bag.add(() => svg.replaceChildren());
  return {
    set: (v) => {
      maxA = v;
      retarget();
    },
    destroy: bag.dispose,
  };
};

export const figure = {
  name: "laptop",
  means:
    "A thin laptop: the pointer coming onto it opens the lid a little further.",
  rules: [1, 3, 8, 9],
  range: [108, 118, 130],
  mount,
};
