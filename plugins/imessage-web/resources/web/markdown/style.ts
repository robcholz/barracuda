/**
 * The rules a reply's Markdown needs, scoped to `.bc-md` (the reply), `.bc-md-*` (its code blocks and
 * tables) and `.bc-tok-*` (syntax colours from `highlight.ts`). They read the portal design system's
 * tokens: `--foreground`, `--muted`, `--muted-foreground`, `--border`, `--link`, `--destructive`,
 * `--font-mono`, `--radius-sm`, `--radius-lg` and the `--blue-900`, `--red-900`, `--lime-900`,
 * `--amber-900` scales, which follow the light and dark themes. A page adds them with its own rules.
 */
export const MARKDOWN_CSS = `
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
.bc-md a[data-incomplete]{cursor:default}
.bc-md a{color:var(--link);text-decoration:underline;text-decoration-color:color-mix(in srgb,currentColor 40%,transparent);text-underline-offset:3px}
.bc-md a:hover{text-decoration-color:currentColor}
.bc-md :not(pre)>code{padding:1px 4px;border-radius:var(--radius-sm);background:var(--muted);font-family:var(--font-mono);font-size:.9em}
.bc-md-table{overflow-x:auto}
.bc-md table{border-collapse:collapse;font-size:13px;line-height:20px}
.bc-md th,.bc-md td{padding:6px 10px;border:1px solid var(--border);text-align:left;vertical-align:top}
.bc-md th{font-weight:600;background:var(--muted)}
.bc-md-code__actions{display:contents}
.bc-md-code[data-incomplete] .bc-md-code__actions>*{opacity:.4;pointer-events:none}
.bc-md-code{border:1px solid var(--border);border-radius:var(--radius-lg);background:var(--muted);overflow:hidden}
.bc-md-code__head{display:flex;align-items:center;justify-content:space-between;gap:8px;min-height:36px;padding:2px 2px 2px 12px;border-bottom:1px solid var(--border);font-family:var(--font-mono);font-size:12px;color:var(--muted-foreground)}
.bc-md-code pre{margin:0;padding:10px 12px;overflow-x:auto;white-space:pre;font-family:var(--font-mono);font-size:12px;line-height:18px}
.bc-tok-k{color:var(--red-900)}
.bc-tok-f,.bc-tok-v{color:var(--blue-900)}
.bc-tok-s{color:var(--lime-900)}
.bc-tok-n,.bc-tok-t{color:var(--amber-900)}
.bc-tok-c{color:var(--muted-foreground);font-style:italic}
.bc-tok-d{color:var(--destructive)}
`;
