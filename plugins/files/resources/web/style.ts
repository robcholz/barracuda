/**
 * The design system's Files rules (`.bc-tree`, `.bc-pane`, `.bc-lines`) and the menu, spinner and
 * shimmer parts this page shares with the design's chat sessions. The shell's stylesheet is held
 * to the device's flash budget and carries none of them, so the page adds them while it is open.
 * Copied from the design system's `components/bundle.css`; keep them in step.
 */
export const STYLE =
  ".bc-tree{flex:none;width:260px;display:flex;flex-direction:column;gap:2px;padding:var(--space-4) var(--space-3);border-right:1px solid var(--border);background:var(--background)}" +
  ".bc-tree__name{flex:1 1 auto;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}" +
  ".bc-pane{flex:1 1 480px;min-width:0;display:flex;flex-direction:column}" +
  ".bc-pane__bar{display:flex;align-items:center;flex-wrap:wrap;gap:8px;min-height:64px;padding:12px var(--space-5)}" +
  ".bc-pane__section{border-top:1px solid var(--border)}" +
  ".bc-pane__body{flex:1 1 auto;min-height:0;display:flex;flex-direction:column;border-top:1px solid var(--border)}" +
  ".bc-pane .bc-expanded__panel{padding-top:var(--space-4)}" +
  ".bc-lines{display:grid;grid-template-columns:40px minmax(0,1fr);row-gap:2px;align-content:start;padding:var(--space-4) var(--space-5) var(--space-6) var(--space-2);overflow:auto}" +
  ".bc-lines__n{padding:1px var(--space-4) 0 0;text-align:right;user-select:none}" +
  ".bc-lines__text{white-space:pre-wrap;overflow-wrap:anywhere}" +
  ".bc-menu-item--destructive{color:var(--destructive)}" +
  ".bc-menu-item--destructive:hover,.bc-menu-item--destructive:active{background:var(--destructive-surface)}" +
  ".bc-menu--confirm{width:256px;padding:12px;display:flex;flex-direction:column;gap:12px}" +
  ".bc-spinner{animation:bc-spin .8s linear infinite}" +
  ".bc-shimmer{color:transparent;background:linear-gradient(90deg,var(--muted-foreground) 35%,var(--foreground) 50%,var(--muted-foreground) 65%) 0 0/300% 100%;-webkit-background-clip:text;background-clip:text;animation:bc-sweep 2s linear infinite}" +
  "@keyframes bc-spin{to{transform:rotate(360deg)}}" +
  "@keyframes bc-sweep{from{background-position:100% 0}to{background-position:0 0}}" +
  "@media (prefers-reduced-motion:reduce){.bc-spinner,.bc-shimmer{animation:none}.bc-shimmer{color:var(--muted-foreground);background:none}}" +
  "@media (max-width:719px){.bc-tree{width:100%;border-right:0;border-bottom:1px solid var(--border)}.bc-lines{grid-template-columns:36px minmax(0,1fr);padding:var(--space-4) var(--space-4) var(--space-6) 0}.bc-lines__n{padding-right:var(--space-3)}}";
