/**
 * Web chat's own rules, from the design system's Chat card: the one-row composer, the fresh
 * conversation, the pinned head, jump to the latest, the spinning mark's tile and the live-state
 * motion. They serve this page alone, so the page adds them on mount and removes them on unmount
 * rather than growing the shell's stylesheet; reduced motion stills every animation.
 */
export const CHAT_CSS = `
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
.bc-mark-tile{display:grid;place-items:center;flex:none;width:16px;height:16px;border-radius:4px;background:var(--signal-foreground)}
.bc-mark-spin{width:13px;height:13px}
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
@media (max-width:719px){.bc-chat-head{padding:var(--space-3) var(--space-4) 0}}
`;
