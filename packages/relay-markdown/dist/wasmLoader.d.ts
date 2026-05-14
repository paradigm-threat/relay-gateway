import type { MdNode } from './mdAst.js';
/** Configure where md2ast.js / md2ast_bg.wasm are served (trailing slash optional). */
export declare function configureRelayMarkdownWasm(baseUrl: string): void;
export declare function parseMarkdownInternal(markdown: string, allowedTags: string[]): Promise<MdNode[]>;
//# sourceMappingURL=wasmLoader.d.ts.map