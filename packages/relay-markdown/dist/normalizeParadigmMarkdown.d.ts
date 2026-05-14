/**
 * paradigm-threat-files convention: `![### Heading` without `](url)` is a mis-prefixed heading.
 * pulldown-cmark treats it as a broken image and swallows following paragraphs.
 */
export declare function normalizeParadigmMarkdown(source: string): string;
//# sourceMappingURL=normalizeParadigmMarkdown.d.ts.map