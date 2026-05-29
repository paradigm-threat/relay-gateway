'use client';
import { jsx as _jsx } from "react/jsx-runtime";
import { useEffect, useMemo, useState } from 'react';
import { defaultHtmlOverrides } from './defaultOverrides.js';
import { renderMdAst } from './mdAst.js';
import { diagnoseMarkdownError, validateMarkdownSource, } from './diagnostics.js';
import { MarkdownCompileErrorPanel } from './MarkdownCompileErrorPanel.js';
import { normalizeParadigmMarkdown } from './normalizeParadigmMarkdown.js';
import { parseMarkdownInternal } from './wasmLoader.js';
export function MarkdownRenderer({ content, overrides = {}, className = 'relay-markdown', sourcePath, anchorProps, anchorOnClick, onReady, }) {
    const [nodes, setNodes] = useState(null);
    const [diagnostics, setDiagnostics] = useState(null);
    const allOverrides = useMemo(() => ({ ...defaultHtmlOverrides, ...overrides }), [overrides]);
    const allowedTags = useMemo(() => Object.keys(allOverrides), [allOverrides]);
    const renderOpts = useMemo(() => ({ overrides: allOverrides, anchorProps, anchorOnClick }), [allOverrides, anchorProps, anchorOnClick]);
    const normalizedContent = useMemo(() => normalizeParadigmMarkdown(content), [content]);
    useEffect(() => {
        let cancel = false;
        (async () => {
            try {
                validateMarkdownSource(normalizedContent, allowedTags);
                const parsed = await parseMarkdownInternal(normalizedContent, allowedTags);
                if (!cancel) {
                    setNodes(parsed);
                    setDiagnostics(null);
                }
            }
            catch (e) {
                if (!cancel) {
                    setNodes(null);
                    setDiagnostics(diagnoseMarkdownError(e, normalizedContent));
                }
            }
        })();
        return () => { cancel = true; };
    }, [normalizedContent, allowedTags]);
    useEffect(() => {
        if (nodes)
            onReady?.();
    }, [nodes, onReady]);
    if (diagnostics) {
        return _jsx(MarkdownCompileErrorPanel, { diagnostics: diagnostics, sourcePath: sourcePath });
    }
    if (!nodes) {
        return _jsx("div", { className: "p-4 text-sm opacity-60", children: "Loading markdown\u2026" });
    }
    return (_jsx("article", { className: className, children: renderMdAst(nodes, renderOpts) }));
}
//# sourceMappingURL=MarkdownRenderer.js.map