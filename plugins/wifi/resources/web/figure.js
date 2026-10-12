// Wi-Fi figure "router", from the approved design (live/src/router.js): the shell's global `HL` is the kernel,
// so this module never imports or bundles it. The shell mounts it for `<hl-figure name="wifi">`; `scan(on)`
// follows the element's `scan` attribute.
// Ported from @lucasmarkes/hairline (MIT) src/figures/router.ts for the Barracuda portal: types stripped, the package's core taken from HL.
const {
  Cam,
  circ,
  clamp,
  disposer,
  facing,
  fit,
  flatDot,
  hull,
  lerp,
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
  ringAt,
  rrect,
  run,
  seg,
  solid,
  spring,
  stepS,
} = HL;
const X1 = 124,
  Y1 = 58,
  H = 14,
  T = 3,
  BR = 13;
const XS = [18, 48, 78, 108],
  YB = 11,
  KH = 5,
  KR = 4.6,
  L = 56,
  R0 = 3.3,
  R1 = 2.1,
  ELBOW = 0.21;
const STOP = R1 + 1,
  MAX = 44,
  REST = [-9, 3, -4, 32],
  LIT0 = 3;
const D = [Math.SQRT1_2, -Math.SQRT1_2];
const falloff = (u, R) => clamp(1 - u / R, 0.1, 1);
const along = (i, th, l) => {
  const s = Math.sin(rad(th)),
    c = Math.cos(rad(th));
  return [XS[i] + D[0] * s * l, YB + D[1] * s * l, H + KH - 1 + c * l];
};
const mount = ({ stage, svg, read }, value) => {
  const bag = disposer();
  let R = value,
    over = null,
    lit = null,
    scanning = false;
  const C = Cam(45, 0.5, 1.78),
    pts = [
      [0, 0, 0],
      [X1, Y1, 0],
      [X1, 0, 0],
      [0, Y1, 0],
    ];
  XS.forEach((_, i) => {
    for (const th of [-MAX, 0, MAX]) pts.push(along(i, th, L + R1));
  });
  fit(C, pts, 200, 166);
  const P = proj(C),
    front = facing(C),
    Pv = (q) => P(q[0], q[1], q[2]);
  const o = P(0, 0, 0),
    dd = P(D[0], D[1], 0),
    zz = P(0, 0, 1);
  const SH = Math.hypot(dd[0] - o[0], dd[1] - o[1]),
    SZ = o[1] - zz[1];
  const g = mk("g", {}, svg);
  const foot = rrect(0, 0, X1, Y1, BR, 6),
    top = rrect(T, T, X1 - T, Y1 - T, BR - T, 6);
  const inner = rrect(
    T + 1.6,
    T + 1.6,
    X1 - T - 1.6,
    Y1 - T - 1.6,
    BR - T - 1.6,
    6,
  );
  put(solid(g), {
    sil: poly(hull(ringAt(P, foot, 0).concat(ringAt(P, top, H)))),
    crease: open(ringAt(P, run(inner, front), H)),
  });
  for (let k = 0; k < 6; k++) {
    const z = H * 0.56,
      y = Y1 - T * (z / H) + 0.1;
    place(
      mk("circle", { r: 1.05, class: k === 0 ? "dot m" : "dot off" }, g),
      P(34 + k * 9, y, z),
    );
  }
  for (let j = 0; j < 3; j++)
    for (let k = 0; k < 13 - (j % 2); k++)
      place(
        flatDot(g, C, 0.5, "dot off"),
        P(26 + (j % 2) * 3 + k * 6, 29 + j * 5.5, H),
      );
  const ants = XS.map((x, i) => {
    const boss = solid(g);
    const shift = (ring) =>
      ring.map((q) => ({ ...q, u: q.u + x, v: q.v + YB }));
    put(
      boss,
      prism(
        P,
        front,
        shift(circ(KR, 16)),
        shift(circ(KR - 1.1, 16)),
        H,
        H + KH,
      ),
    );
    return {
      i,
      el: solid(g),
      sp: spring(REST[i], { eps: 0.05 }),
      drawn: NaN,
      piv: Pv(along(i, 0, 0)),
    };
  });
  const gap = Math.abs(ants[1].piv[0] - ants[0].piv[0]) / SH;
  const disc = (p, r) =>
    Array.from({ length: 20 }, (_, k) => [
      p[0] + r * SH * Math.cos((k * Math.PI) / 10),
      p[1] + r * SH * Math.sin((k * Math.PI) / 10),
    ]);
  function drawAnt(a) {
    const th = a.sp.x;
    if (th === a.drawn) return;
    a.drawn = th;
    const b = Pv(along(a.i, th, 0)),
      t = Pv(along(a.i, th, L)),
      e = Pv(along(a.i, th, L * ELBOW));
    const len = Math.hypot(t[0] - b[0], t[1] - b[1]),
      n = [-(t[1] - b[1]) / len, (t[0] - b[0]) / len];
    const w = lerp(R0, R1, ELBOW) * SH - 1.1;
    put(a.el, {
      sil: poly(hull(disc(b, R0).concat(disc(t, R1)))),
      crease: seg(
        [e[0] + n[0] * w, e[1] + n[1] * w],
        [e[0] - n[0] * w, e[1] - n[1] * w],
      ),
    });
  }
  function light(a) {
    if (a === lit) return;
    lit?.el.sil.classList.remove("hi");
    lit = a;
    a.el.sil.classList.add("hi");
  }
  // While the device scans, the antennas sweep in a wave, one after another: an ambient loop, so it runs only
  // while the figure is on screen (the kernel's loop sleeps offscreen) and lands at rest under reduced motion.
  // neighbours stay within a few degrees of each other, so no two antennas ever cross
  const SWEEP = 2.2,
    LEAN = 20,
    LAG = 0.55;
  const B = register(stage, (dt, now) => {
    if (scanning && !over)
      ants.forEach((a, i) => {
        a.sp.t = LEAN * Math.sin((now / 1000 / SWEEP) * 2 * Math.PI - i * LAG);
      });
    let m = scanning && !over;
    for (const a of ants) {
      if (stepS(a.sp, dt)) m = true;
      drawAnt(a);
    }
    return m;
  });
  bag.add(B.unregister);
  function retarget() {
    if (!over) {
      ants.forEach((a, i) => {
        a.sp.t = REST[i];
      });
      light(ants[LIT0]);
      read.textContent = "rest";
    } else {
      const at = over;
      const dxs = ants.map((a) => (at[0] - a.piv[0]) / SH),
        d0 = Math.min(...dxs.map(Math.abs));
      const near = ants[dxs.findIndex((d) => Math.abs(d) === d0)];
      ants.forEach((a, i) => {
        const dx = Math.sign(dxs[i]) * Math.max(Math.abs(dxs[i]) - STOP, 0),
          hz = Math.max(
            (a.piv[1] - at[1]) / SZ,
            Math.sqrt(Math.max(L * L - dx * dx, 0)),
            0,
          );
        const aim = (Math.atan2(dx, hz) * 180) / Math.PI;
        a.sp.t =
          clamp(aim, -MAX, MAX) * falloff((Math.abs(dxs[i]) - d0) / gap, R);
      });
      light(near);
      read.textContent = `antenna ${near.i + 1} · ${Math.round(Math.abs(near.sp.t))}°`;
    }
    B.wake();
  }
  light(ants[LIT0]);
  bag.add(
    pointer(stage, {
      move: (p) => {
        over = p;
        retarget();
      },
      leave: () => {
        over = null;
        retarget();
      },
    }),
  );
  bag.add(() => svg.replaceChildren());
  return {
    set: (v) => {
      R = v;
      if (over) retarget();
    },
    scan: (on) => {
      if (scanning === !!on) return;
      scanning = !!on;
      if (!scanning) retarget();
      B.wake();
    },
    destroy: bag.dispose,
  };
};

export const figure = {
  name: "router",
  means:
    "A wifi router: its antennas lean toward the pointer; while it scans, they sweep in a wave.",
  rules: [1, 3, 7, 8],
  range: [0.5, 1.5, 3],
  mount,
};
