export type MarkdownCompileErrorKind = 'unknown_tag' | 'malformed_tag' | 'unclosed_tag' | 'mdx_syntax' | 'wasm' | 'json' | 'runtime';
export interface MarkdownCompileDiagnostics {
    kind: MarkdownCompileErrorKind;
    headline: string;
    line: number | null;
    column: number | null;
    codeFrame: string | null;
    fullMessage: string;
    hint: string | null;
}
export declare class MarkdownCompileError extends Error {
    readonly diagnostics: MarkdownCompileDiagnostics;
    constructor(diagnostics: MarkdownCompileDiagnostics);
}
/** Scan markdown before WASM parse; throws {@link MarkdownCompileError} on blocking issues. */
export declare function validateMarkdownSource(source: string, allowedTags: string[]): void;
export declare function diagnoseMarkdownError(error: unknown, source: string): MarkdownCompileDiagnostics;
//# sourceMappingURL=diagnostics.d.ts.map