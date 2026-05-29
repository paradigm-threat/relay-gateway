import type { MdNode } from './mdAst.js';
/** Configure where md2ast.js / md2ast_bg.wasm are served (trailing slash optional). */
export declare function configureMd2astWasm(baseUrl: string): void;
/** @deprecated Use configureMd2astWasm */
export declare const configureRelayMarkdownWasm: typeof configureMd2astWasm;
export declare function parseMarkdownInternal(markdown: string, allowedTags: string[]): Promise<MdNode[]>;
//# sourceMappingURL=wasmLoader.d.ts.map