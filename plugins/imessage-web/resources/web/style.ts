/**
 * Web chat's own rules, from the design system's Chat card: the one-row composer, the fresh
 * conversation, the pinned head, jump to the latest, the session rail, icon-button tooltips, the spinning mark's box,
 * the live-state motion (a reply's Markdown brings its own, `MARKDOWN_CSS`), and the Attachment, Media and Viewer
 * cards for files in the composer and the log.
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
.bc-attachments{display:flex;flex-wrap:wrap;gap:8px;padding:8px 8px 0 12px}
.bc-attachment{position:relative;flex:none;width:64px;height:64px;border:1px solid var(--border);border-radius:var(--radius-md);overflow:hidden;background:var(--muted)}
.bc-attachment>img{display:block;width:100%;height:100%;object-fit:cover}
.bc-attachment--file{width:auto;max-width:240px;display:flex;align-items:center;gap:8px;padding:0 32px 0 12px}
.bc-attachment--error{border-color:var(--destructive)}
.bc-attachment--busy>img,.bc-attachment--error>img{opacity:.35}
.bc-attachment__name{min-width:0;display:flex;flex-direction:column}
.bc-attachment__name>.bc-small{white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.bc-attachment__state{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;color:var(--foreground)}
.bc-attachment__retry{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);color:var(--foreground)}
.bc-attachment__remove{position:absolute;top:2px;right:2px;background:var(--background)}
.bc-attachment__remove:hover{background:var(--muted-hover)}
.bc-attachment__remove:active{background:var(--muted-active)}
.bc-attachment--file>.bc-attachment__remove{top:50%;transform:translateY(-50%)}
.bc-attachment__progress{position:absolute;left:0;right:0;bottom:0;height:2px;background:var(--border)}
.bc-attachment__progress>i{display:block;height:100%;background:var(--signal)}
.bc-composer--drop,.bc-composer--drop:hover{border-style:dashed;border-color:var(--ring)}
.bc-drop{display:flex;align-items:center;justify-content:center;gap:8px;min-height:88px;margin:6px 6px 0;border-radius:var(--radius-sm);background:var(--muted)}
.bc-media{position:relative;display:block;padding:0;border:1px solid var(--border);border-radius:var(--radius-lg);overflow:hidden;background:var(--muted);cursor:zoom-in;transition:border-color .1s}
.bc-media>img,.bc-media>video{display:block;width:100%;height:100%;object-fit:cover}
.bc-media:hover{border-color:var(--border-hover)}
.bc-media:focus-visible{outline:none;border-color:var(--ring)}
.bc-media-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:1px;border:1px solid var(--border);border-radius:var(--radius-lg);overflow:hidden;background:var(--border)}
.bc-media-grid>.bc-media{border:0;border-radius:0}
.bc-media-grid>.bc-media:hover{outline:1px solid var(--border-hover);outline-offset:-1px}
.bc-media-grid>.bc-media:focus-visible{outline:2px solid var(--ring);outline-offset:-2px}
.bc-media--more>img{opacity:.35}
.bc-media__more{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;color:var(--foreground)}
.bc-media--video{cursor:default}
.bc-media__play{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%)}
.bc-track{position:relative;display:block;height:2px;background:var(--border)}
.bc-track>i{position:absolute;left:0;top:0;bottom:0;background:var(--foreground)}
.bc-card--progress{position:relative;overflow:hidden}
.bc-card--progress::after{content:"";position:absolute;left:0;bottom:0;height:2px;width:var(--progress,0%);background:var(--signal)}
.bc-viewer{position:fixed;inset:0;z-index:40;display:flex;flex-direction:column;background:var(--background);color:var(--foreground)}
.bc-viewer__bar{display:flex;align-items:center;gap:12px;height:var(--topbar-height);padding:0 var(--space-4);border-bottom:1px solid var(--border)}
.bc-viewer__stage{position:relative;flex:1 1 auto;min-height:0;display:flex;align-items:center;justify-content:center;padding:var(--space-8) 96px}
.bc-viewer__image{max-width:100%;max-height:100%;border:1px solid var(--border);border-radius:var(--radius-lg);overflow:hidden}
.bc-viewer__image>img{display:block;max-width:100%;max-height:calc(100vh - 120px);object-fit:contain}
.bc-viewer__nav{position:absolute;top:50%;transform:translateY(-50%)}
@media (max-width:719px){.bc-viewer__stage{padding:var(--space-4)}.bc-viewer__nav{display:none}}
`;
