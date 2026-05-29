import { type AnchorHTMLAttributes, type ComponentType, type MouseEventHandler, type ReactNode } from 'react';
export type MdNode = {
    type: 'text';
    content: string;
} | {
    type: 'element';
    tag: string;
    props: Record<string, unknown>;
    children: MdNode[];
};
export type MdOverride = string | ComponentType<Record<string, unknown>> | {
    component: string | ComponentType<Record<string, unknown>>;
    props?: Record<string, unknown>;
};
export type MdOverrides = Record<string, MdOverride>;
export interface RenderMdAstOpts {
    overrides?: MdOverrides;
    anchorProps?: AnchorHTMLAttributes<HTMLAnchorElement>;
    anchorOnClick?: MouseEventHandler<HTMLAnchorElement>;
}
export declare function renderMdAst(nodes: MdNode[], opts?: RenderMdAstOpts): ReactNode[];
export declare function renderMdAstNode(n: MdNode, i: number, opts: RenderMdAstOpts | undefined, overrides: MdOverrides): ReactNode;
//# sourceMappingURL=mdAst.d.ts.map