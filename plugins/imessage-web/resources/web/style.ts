/**
 * Web chat's own rules, from the design system's Chat card: the one-row composer, the fresh
 * conversation, the pinned head, jump to the latest, the session rail, icon-button tooltips, the spinning mark's box,
 * the live-state motion, and a reply's Markdown (`.bc-md`) with its code's colours (`.bc-tok-*`, from `highlight.ts`).
 * They serve this page alone, so the page adds them on mount and removes them on unmount
 * rather than growing the shell's stylesheet; reduced motion stills every animation.
 */
export const CHAT_CSS = `
.bc-icon-button,.bc-button--icon{position:relative}
.bc-icon-button:hover>.bc-tooltip,.bc-icon-button:focus-visible>.bc-tooltip,.bc-button--icon:hover>.bc-tooltip,.bc-button--icon:focus-visible>.bc-tooltip{display:block}
.bc-composer__row{display:flex;align-items:flex-end;gap:8px}
.bc-composer__row>textarea{flex:1 1 auto;min-width:0;min-height:44px;max-height:200px;padding:11px 12px;overflow-y:auto;field-sizing:content}
.bc-composer__actions{flex:none;display:flex;align-items:center;gap:8px;padding:6px 6px 6px 0}
.bc-composer__text{flex:1 1 auto;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.bc-chat--fresh .bc-chat-log{flex:1 1 0;display:flex;flex-direction:column;justify-content:flex-end}
.bc-chat--fresh .bc-chat-dock{flex:1 1 0;position:static}
.bc-chat-head{position:sticky;top:0;z-index:5;display:flex;justify-content:center;padding:var(--space-3) var(--space-8) 0;background:var(--background)}
.bc-chat-head .bc-badge{position:relative;cursor:default}
.bc-chat-head .bc-badge .bc-icon{width:14px;height:14px}
.bc-chat-head .bc-badge:hover>.bc-tooltip,.bc-chat-head .bc-badge:focus-visible>.bc-tooltip{display:block}
.bc-chat-head .bc-badge:focus-visible{outline:2px solid var(--ring);outline-offset:2px}
.bc-chat-jump{position:absolute;left:50%;top:-52px;transform:translateX(-50%)}
.bc-mark-tile{display:grid;place-items:center;flex:none;width:16px;height:16px}
.bc-mark-spin{width:16px;height:16px}
.bc-chat-rail{flex:none;width:260px;position:sticky;top:0;align-self:flex-start;height:calc(100vh - var(--topbar-height));display:flex;flex-direction:column;gap:var(--space-4);padding:var(--space-4) var(--space-3);border-right:1px solid var(--border);background:var(--background)}
.bc-chat-rail__list{flex:1 1 auto;min-height:0;overflow-y:auto;display:flex;flex-direction:column;gap:16px}
.bc-chat-rail__foot{margin:0;padding:0 8px}
.bc-session{position:relative}
.bc-session>.bc-nav-item{padding-right:36px}
.bc-session__title{flex:1 1 auto;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.bc-session__more{position:absolute;right:6px;top:6px;opacity:0}
.bc-session:hover .bc-session__more,.bc-session:focus-within .bc-session__more,.bc-session__more[aria-expanded="true"]{opacity:1}
.bc-session:hover .bc-spinner,.bc-session:focus-within .bc-spinner,.bc-session:has([aria-expanded="true"]) .bc-spinner{visibility:hidden}
.bc-session__rename{width:100%;height:36px}
.bc-menu-item--destructive{color:var(--destructive)}
.bc-menu-item--destructive:hover,.bc-menu-item--destructive:active{background:var(--destructive-surface)}
.bc-menu--confirm{width:256px;padding:12px;display:flex;flex-direction:column;gap:12px}
.bc-chat-rail .bc-menu--confirm{left:0;right:0;width:auto}
.bc-chat-rail-toggle{display:none}
.bc-md>*,.bc-md blockquote>*,.bc-md li>*{margin:0}
.bc-md>*+*,.bc-md blockquote>*+*,.bc-md li>*+*{margin-top:8px}
.bc-md h1,.bc-md h2,.bc-md h3,.bc-md h4,.bc-md h5,.bc-md h6{font-size:15px;line-height:22px;font-weight:600}
.bc-md h1{font-size:20px;line-height:28px}
.bc-md h2{font-size:17px;line-height:24px}
.bc-md>:is(h1,h2,h3,h4,h5,h6):not(:first-child){margin-top:16px}
.bc-md ul,.bc-md ol{padding-left:24px}
.bc-md li+li{margin-top:4px}
.bc-md li::marker{color:var(--muted-foreground)}
.bc-md .bc-md-task{list-style:none}
.bc-md .bc-md-task input{margin:0 6px 0 -20px;vertical-align:-2px}
.bc-md blockquote{padding-left:12px;border-left:2px solid var(--border);color:var(--muted-foreground)}
.bc-md hr{height:0;border:0;border-top:1px solid var(--border)}
.bc-md a{color:var(--link);text-decoration:underline;text-decoration-color:color-mix(in srgb,currentColor 40%,transparent);text-underline-offset:3px}
.bc-md a:hover{text-decoration-color:currentColor}
.bc-md :not(pre)>code{padding:1px 4px;border-radius:var(--radius-sm);background:var(--muted);font-family:var(--font-mono);font-size:.9em}
.bc-md-table{overflow-x:auto}
.bc-md table{border-collapse:collapse;font-size:13px;line-height:20px}
.bc-md th,.bc-md td{padding:6px 10px;border:1px solid var(--border);text-align:left;vertical-align:top}
.bc-md th{font-weight:600;background:var(--muted)}
.bc-md-code{border:1px solid var(--border);border-radius:var(--radius-lg);background:var(--muted);overflow:hidden}
.bc-md-code__head{display:flex;align-items:center;justify-content:space-between;gap:8px;min-height:36px;padding:2px 2px 2px 12px;border-bottom:1px solid var(--border);font-family:var(--font-mono);font-size:12px;color:var(--muted-foreground)}
.bc-md-code pre{margin:0;padding:10px 12px;overflow-x:auto;white-space:pre;font-family:var(--font-mono);font-size:12px;line-height:18px}
.bc-tok-k{color:var(--red-900)}
.bc-tok-f,.bc-tok-v{color:var(--blue-900)}
.bc-tok-s{color:var(--lime-900)}
.bc-tok-n,.bc-tok-t{color:var(--amber-900)}
.bc-tok-c{color:var(--muted-foreground);font-style:italic}
.bc-tok-d{color:var(--destructive)}
@keyframes bc-blink{50%{opacity:0}}
@keyframes bc-dot{0%,80%,100%{opacity:.3;transform:none}40%{opacity:1;transform:translateY(-2px)}}
@keyframes bc-sweep{from{background-position:100% 0}to{background-position:0 0}}
@keyframes bc-spin{to{transform:rotate(360deg)}}
@keyframes bc-rise{from{opacity:0;transform:translateY(4px)}}
.bc-caret{animation:bc-blink 1s step-end infinite}
.bc-typing>i{animation:bc-dot 1.2s ease-in-out infinite}
.bc-typing>i:nth-child(2){animation-delay:.15s}
.bc-typing>i:nth-child(3){animation-delay:.3s}
.bc-shimmer{color:transparent;background:linear-gradient(90deg,var(--muted-foreground) 35%,var(--foreground) 50%,var(--muted-foreground) 65%) 0 0/300% 100%;-webkit-background-clip:text;background-clip:text;animation:bc-sweep 2s linear infinite}
.bc-spinner{animation:bc-spin .8s linear infinite}
.bc-turn--new{animation:bc-rise .15s ease-out both}
@media (prefers-reduced-motion:reduce){.bc-caret,.bc-typing>i,.bc-spinner,.bc-turn--new,.bc-shimmer{animation:none}.bc-shimmer{color:var(--muted-foreground);background:none}}
@media (max-width:719px){.bc-chat-head{padding:var(--space-3) var(--space-4) 0}.bc-chat-rail{display:none;position:fixed;inset:0 auto 0 0;height:100vh;z-index:40;box-shadow:var(--shadow-xs)}.bc-chat-rail--open{display:flex}.bc-chat-rail-toggle{display:inline-flex}}
`;
