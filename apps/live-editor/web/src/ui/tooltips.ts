/**
 * Tooltips drawn above the page. The design system's tooltips are positioned inside their control,
 * so the sidebar, the view columns and the console, which scroll and are containers, clip or cover
 * them. Each `.bc-tooltip` stays where it is and its rules still decide when it shows; app.css only
 * makes it invisible, and this layer draws a copy on top, kept on screen.
 */

const GAP = 6;
const EDGE = 8;

type Side = "above" | "below" | "right";

/** The side the design system puts `tip` on: right for the sidebar, below in a top bar, else above. */
function side(tip: HTMLElement): Side {
  if (tip.classList.contains("bc-tooltip--right")) return "right";
  return tip.closest(".bc-topbar") ? "below" : "above";
}

function place(float: HTMLElement, owner: DOMRect, tip: HTMLElement) {
  const { innerWidth: width, innerHeight: height } = window;
  const box = float.getBoundingClientRect();
  let want = side(tip);
  if (want === "right" && owner.right + GAP + box.width > width - EDGE)
    want = "above";
  if (want === "above" && owner.top - GAP - box.height < EDGE) want = "below";
  if (want === "below" && owner.bottom + GAP + box.height > height - EDGE)
    want = "above";
  let left: number;
  let top: number;
  if (want === "right") {
    left = owner.right + GAP + 2;
    top = owner.top + (owner.height - box.height) / 2;
  } else {
    left = tip.classList.contains("bc-tooltip--start")
      ? owner.left
      : owner.left + (owner.width - box.width) / 2;
    top = want === "above" ? owner.top - GAP - box.height : owner.bottom + GAP;
  }
  left = Math.min(Math.max(left, EDGE), width - EDGE - box.width);
  top = Math.min(Math.max(top, EDGE), height - EDGE - box.height);
  float.style.left = `${Math.round(left)}px`;
  float.style.top = `${Math.round(top)}px`;
}

/** Shows each tooltip under `root` in a layer above the page while its control is hovered or focused. */
export function floatTooltips(root: HTMLElement) {
  const float = document.createElement("span");
  float.setAttribute("aria-hidden", "true");
  float.className = "bc-tooltip le-float-tip";
  document.body.append(float);
  let owner: HTMLElement | null = null;
  let tip: HTMLElement | null = null;
  /** Set by a scroll, which moves controls under a still pointer; the next move finds its control. */
  let moved = false;

  const hide = () => {
    owner = tip = null;
    float.hidden = true;
  };
  const show = () => {
    // The design system's rules decide: a tooltip they keep at display:none stays hidden.
    if (
      !owner ||
      !tip ||
      !owner.isConnected ||
      getComputedStyle(tip).display === "none"
    )
      return hide();
    float.className = `${tip.className} le-float-tip`;
    float.textContent = tip.textContent;
    const theme = owner.closest("[data-theme]")?.getAttribute("data-theme");
    if (theme) float.setAttribute("data-theme", theme);
    else float.removeAttribute("data-theme");
    float.hidden = false;
    place(float, owner.getBoundingClientRect(), tip);
  };
  /** The control whose own tooltip `target` is in or on. */
  const find = (target: EventTarget | null) => {
    let at = target instanceof Element ? target : null;
    while (at && at !== root.parentElement) {
      const own = [...at.children].find((child) =>
        child.classList.contains("bc-tooltip"),
      );
      if (own && at instanceof HTMLElement)
        return { at, own: own as HTMLElement };
      at = at.parentElement;
    }
    return null;
  };
  const enter = (target: EventTarget | null) => {
    const found = find(target);
    if (!found) return hide();
    owner = found.at;
    tip = found.own;
    // Hover and focus styles apply in the same frame; read them after.
    requestAnimationFrame(show);
  };

  hide();
  root.addEventListener("pointerover", (event) => enter(event.target));
  root.addEventListener("pointermove", (event) => {
    if (!moved) return;
    moved = false;
    enter(event.target);
  });
  root.addEventListener("pointerleave", hide);
  root.addEventListener("focusin", (event) => enter(event.target));
  root.addEventListener("focusout", hide);
  // A control that re-renders or moves takes its tooltip with it.
  root.addEventListener("pointerdown", () => requestAnimationFrame(show));
  addEventListener(
    "scroll",
    () => {
      hide();
      moved = true;
    },
    true,
  );
  addEventListener("resize", hide);
}
