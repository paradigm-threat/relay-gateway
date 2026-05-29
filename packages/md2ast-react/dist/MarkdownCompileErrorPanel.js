'use client';
import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
const KIND_LABEL = {
    unknown_tag: 'Unknown component tag',
    malformed_tag: 'Malformed tag',
    unclosed_tag: 'Unclosed tag',
    mdx_syntax: 'Unsupported MDX / JSX syntax',
    wasm: 'Runtime (WASM)',
    json: 'Parse error',
    runtime: 'Render error',
};
export function MarkdownCompileErrorPanel({ diagnostics, sourcePath, }) {
    const loc = diagnostics.line != null
        ? `Line ${diagnostics.line}${diagnostics.column != null ? `, column ${diagnostics.column + 1}` : ''}`
        : null;
    return (_jsxs("div", { className: "relay-markdown-error not-prose rounded-2xl border border-red-300/80 bg-red-50/90 p-6 text-left shadow-sm dark:border-red-900/60 dark:bg-red-950/40", role: "alert", children: [_jsxs("div", { className: "flex flex-wrap items-baseline justify-between gap-2 border-b border-red-200/80 pb-3 dark:border-red-900/50", children: [_jsx("h2", { className: "text-lg font-semibold text-red-900 dark:text-red-100", children: "Markdown compile error" }), _jsx("span", { className: "text-xs font-mono uppercase tracking-wide text-red-700/80 dark:text-red-300/80", children: KIND_LABEL[diagnostics.kind] })] }), sourcePath ? (_jsxs("p", { className: "mt-3 text-sm text-red-900/90 dark:text-red-100/90", children: [_jsx("span", { className: "font-medium", children: "Source:" }), ' ', _jsx("code", { className: "rounded bg-red-100/80 px-1.5 py-0.5 text-xs dark:bg-red-900/50", children: sourcePath })] })) : null, loc ? (_jsx("p", { className: "mt-2 text-sm font-medium text-red-800 dark:text-red-200", children: loc })) : null, _jsx("p", { className: "mt-3 text-sm leading-relaxed text-red-950 dark:text-red-50", children: diagnostics.headline }), diagnostics.codeFrame ? (_jsxs("div", { className: "mt-4", children: [_jsx("p", { className: "mb-1 text-xs font-semibold uppercase tracking-wide text-red-800/80 dark:text-red-200/80", children: "Markdown excerpt" }), _jsx("pre", { className: "max-h-[min(24rem,50vh)] overflow-auto rounded-xl border border-red-200/70 bg-white/90 p-4 font-mono text-xs leading-relaxed text-slate-900 dark:border-red-900/50 dark:bg-slate-950/80 dark:text-slate-100", children: diagnostics.codeFrame })] })) : null, diagnostics.hint ? (_jsxs("p", { className: "mt-4 text-xs leading-relaxed text-red-800/90 dark:text-red-200/90", children: [_jsx("strong", { children: "Hint:" }), " ", diagnostics.hint] })) : null, _jsxs("details", { className: "mt-4 text-sm text-red-900/85 dark:text-red-100/85", children: [_jsx("summary", { className: "cursor-pointer select-none font-medium text-red-800 dark:text-red-200", children: "Full message" }), _jsx("pre", { className: "mt-2 max-h-[min(20rem,40vh)] overflow-auto whitespace-pre-wrap break-words rounded-lg border border-red-200/60 bg-white/70 p-3 font-mono text-xs dark:border-red-900/40 dark:bg-slate-950/60", children: diagnostics.fullMessage })] })] }));
}
//# sourceMappingURL=MarkdownCompileErrorPanel.js.map