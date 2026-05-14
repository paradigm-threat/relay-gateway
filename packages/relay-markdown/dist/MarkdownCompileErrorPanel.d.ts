import type { MarkdownCompileDiagnostics } from './diagnostics.js';
export interface MarkdownCompileErrorPanelProps {
    diagnostics: MarkdownCompileDiagnostics;
    /** Optional path label (repo-relative or URL). */
    sourcePath?: string;
}
export declare function MarkdownCompileErrorPanel({ diagnostics, sourcePath, }: MarkdownCompileErrorPanelProps): import("react/jsx-runtime").JSX.Element;
//# sourceMappingURL=MarkdownCompileErrorPanel.d.ts.map