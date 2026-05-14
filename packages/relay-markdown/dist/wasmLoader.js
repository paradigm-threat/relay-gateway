let wasmBaseUrl = '/wasm/md2ast/';
let wasmMod = null;
let wasmReady = null;
/** Configure where md2ast.js / md2ast_bg.wasm are served (trailing slash optional). */
export function configureRelayMarkdownWasm(baseUrl) {
    wasmBaseUrl = baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`;
}
async function loadWasm() {
    if (wasmMod)
        return wasmMod;
    if (!wasmReady) {
        wasmReady = (async () => {
            const entry = `${wasmBaseUrl}md2ast.js`;
            const mod = (await import(/* webpackIgnore: true */ entry));
            await mod.default();
            wasmMod = mod;
            return mod;
        })();
    }
    return wasmReady;
}
export async function parseMarkdownInternal(markdown, allowedTags) {
    const mod = await loadWasm();
    const json = mod.transpile(markdown, allowedTags);
    return JSON.parse(json);
}
//# sourceMappingURL=wasmLoader.js.map