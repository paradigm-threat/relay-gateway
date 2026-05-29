import type { AnchorHTMLAttributes, ComponentType, MouseEventHandler } from 'react';
import { type MdNode, type MdOverrides } from './mdAst.js';
export type { MdNode, MdOverrides };
export type { MarkdownCompileDiagnostics } from './diagnostics.js';
export type MarkdownOverrideComponent = ComponentType<Record<string, unknown>>;
export interface MarkdownRendererProps {
    content: string;
    overrides?: MdOverrides;
    className?: string;
    /** Shown in compile error panel (e.g. repo path). */
    sourcePath?: string;
    anchorProps?: AnchorHTMLAttributes<HTMLAnchorElement>;
    anchorOnClick?: MouseEventHandler<HTMLAnchorElement>;
    /** Called when render finishes (success or error panel). */
    onReady?: () => void;
}
export declare function MarkdownRenderer({ content, overrides, className, sourcePath, anchorProps, anchorOnClick, onReady, }: MarkdownRendererProps): import("react/jsx-runtime").JSX.Element;
//# sourceMappingURL=MarkdownRenderer.d.ts.map