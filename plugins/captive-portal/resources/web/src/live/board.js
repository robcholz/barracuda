// Barracuda built-in figure "board", from the approved design (live/src/board.js); the kernel comes from ./kernel.js.
import HL from "./kernel.js";

/**
 * Board: the Barracuda device, a small dev board. A shielded radio module with
 * its antenna trace, a USB-C port at the near end, two buttons, a regulator,
 * two status LEDs, and a header strip of thirteen pins down each long edge.
 * The board is always on: its power LED is lit green and steady, its activity
 * LED blinks red, and from the antenna trace radio ripples spread out over the
 * board, three at a time, passing under every part that stands on it. The
 * module's shield can is the bright mark. The pointer changes nothing: the
 * board runs the same whether or not anyone is looking at it; only the
 * read-out names it. The slider is how long one ripple takes, in ms.
 *
 * The pattern: an ambient loop, so it runs only while the board is on screen
 * (the kernel's loop sleeps offscreen) and, under reduced motion, holds still:
 * no ripples, both LEDs lit. The ripples lie on the board's top and are painted
 * right after it, so the strips, the module and the port cover them (rule 06).
 * The LEDs' two colours are the page's (led-g, led-r): the figure only names them.
 */
const {
  Cam, circ, facing, fit, open, poly, prism, proj, rings, rrect, reducedMotion,
  disposer, flatDot, mk, place, pointer, put, register, solid,
} = HL;

const L = 130, W = 70, T = 2.4, N = 13, PITCH = 8, X0 = 18, SB = 3.4, PIN = 5;
const ROWS = [7, 63];
const CZ = 1.2, CH = 4, CAN = [42, 19, 76, 45], ANT = [88, 32], REACH = 64, WAVES = 3;  // the ripples: where they start, how far they spread, how many at once

function box(g, P, front, x0, y0, x1, y1, r, b, z0, z1) {
  const [ring, inner] = rings(x0, y0, x1, y1, r, b);
  const s = solid(g);
  put(s, prism(P, front, ring, inner, z0, z1));
  return s;
}

function mount({ stage, svg, read }, value) {
  const bag = disposer();
  let cycle = value;
  const C = Cam(45, 0.5, 2.2);
  fit(C, [[0, 0, -T], [L + 4, W, -T], [L + 4, 0, -T], [0, W, -T], [18, 0, SB + 14]], 200, 172);
  const P = proj(C), front = facing(C);
  const g = mk("g", {}, svg);

  box(g, P, front, 0, 0, L, W, 6, 1.6, -T, 0);
  // the ripples, on the board's top, under everything that stands on it
  const waves = Array.from({ length: WAVES }, () => mk("path", { class: "nf sil" }, g));
  const ring = circ(1, 96);
  // mounting holes in the four corners
  for (const [hx, hy] of [[5, 5], [L - 5, 5], [5, W - 5], [L - 5, W - 5]]) {
    mk("path", { d: poly(rrect(hx - 1.8, hy - 1.8, hx + 1.8, hy + 1.8, 1.8, 4).map((q) => P(q.u, q.v, 0))), class: "nf lo" }, g);
  }

  // a header strip: its housing and thirteen pins, each too small to carry a crease (rule 09, draw less)
  function strip(yc) {
    const sg = mk("g", {}, g);
    box(sg, P, front, X0 - 5, yc - 3, X0 + (N - 1) * PITCH + 5, yc + 3, 1.2, 0.5, 0, SB);
    for (let i = 0; i < N; i++) {
      const x = X0 + i * PITCH;
      const [ring] = rings(x - 1.4, yc - 1.4, x + 1.4, yc + 1.4, 0.6, 0.35);
      put(solid(sg), prism(P, front, ring, null, SB, SB + PIN));
    }
  }
  strip(ROWS[0]);

  // buttons at the far end
  for (const by of [22, 48]) {
    box(g, P, front, 6, by - 3, 12, by + 3, 1, 0.4, 0, 1.8);
    box(g, P, front, 7.6, by - 1.4, 10.4, by + 1.4, 1.2, 0.4, 1.8, 3);
  }
  // the module: its carrier, the shield can, and the antenna trace on the bare end
  box(g, P, front, 38, 16, 96, 48, 2, 0.6, 0, CZ);
  const can = box(g, P, front, CAN[0], CAN[1], CAN[2], CAN[3], 2.4, 1, CZ, CZ + CH);
  can.sil.classList.add("hi");
  const meander = [];
  for (let k = 0; k < 5; k++) { const y = k % 2 ? 45 : 19, x = 81 + k * 3; meander.push(P(x, k % 2 ? 19 : 45, CZ), P(x, y, CZ)); }
  mk("path", { d: open(meander), class: "nf lo" }, g);
  // the regulator and the status LED
  box(g, P, front, 102, 40, 112, 50, 1, 0.4, 0, 2);
  const power = flatDot(g, C, 1.2, "dot led-g"), activity = flatDot(g, C, 1.2, "dot led-r");
  // side by side across the board, parallel to its near end and the USB-C port on it
  place(power, P(108, 34, 0));
  place(activity, P(108, 28, 0));
  // the USB-C port at the near end, its opening on the end face
  box(g, P, front, L - 9, 29, L + 4, 41, 2.4, 0.6, 0.3, 4.6);
  mk("path", { d: poly(rrect(31.5, 1.3, 38.5, 3.6, 1.1, 4).map((q) => P(L + 4, q.u, q.v))), class: "nf" }, g);
  strip(ROWS[1]);

  // A ripple's radius over its life: fast out of the antenna, slowing as it spreads (an ease-out, never linear).
  const grow = (p) => REACH * (1 - (1 - p) * (1 - p));
  // a ripple is kept to the board's top: the arcs of it that lie on the board, a little in from the edge
  const onBoard = (x, y) => x > 2 && x < L - 2 && y > 2 && y < W - 2;
  const rip = (r) => {
    const runs = [];
    let cur = null;
    for (let k = 0; k <= ring.length; k++) {
      const q = ring[k % ring.length], x = ANT[0] + q.u * r, y = ANT[1] + q.v * r;
      if (onBoard(x, y)) (cur ??= (runs.push([]), runs[runs.length - 1])).push(P(x, y, 0));
      else cur = null;
    }
    if (runs.length > 1 && onBoard(ANT[0] + ring[0].u * r, ANT[1] + ring[0].v * r)) runs[0] = runs.pop().concat(runs[0]);
    return runs.filter((p) => p.length > 1).map(open).join("");
  };
  // Time runs as phase, accumulated frame by frame, so a change of the slider never makes a ripple jump.
  let phase = 0, flash = 0, blink = null;
  const B = register(stage, (dt) => {
    const still = reducedMotion();
    phase += (dt * 1000) / cycle;
    flash += (dt * 1000) / 450;
    const lit = still || Math.floor(flash) % 2 === 0;           // the activity LED: on, off, on, off
    if (lit !== blink) { blink = lit; activity.classList.toggle("off", !lit); }
    waves.forEach((el, k) => {
      const age = phase - k / WAVES;                            // this ripple's age, in lives
      el.setAttribute("d", still || age < 0 ? "" : rip(grow(age % 1)));
    });
    return !still;
  });
  bag.add(B.unregister);
  read.textContent = "rest";
  // the pointer is heard, and only named: the animation is the same with it or without it
  bag.add(pointer(stage, { move: () => { read.textContent = "board"; }, leave: () => { read.textContent = "rest"; } }));
  bag.add(() => svg.replaceChildren());

  return {
    set: (v) => { cycle = v; },
    destroy: bag.dispose,
  };
}

export const figure = {
  name: "board",
  means: "The Barracuda board, always on: power LED steady, activity LED blinking, radio ripples from the antenna.",
  rules: [5, 6, 7, 9],
  range: [2200, 1600, 1100],
  mount,
};
