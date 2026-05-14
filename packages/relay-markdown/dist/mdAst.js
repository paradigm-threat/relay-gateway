import { jsx as _jsx } from "react/jsx-runtime";
import { createElement } from 'react';
function resolveOverride(overrides, tag) {
    const override = overrides[tag];
    if (!override)
        return { component: tag };
    if (typeof override === 'string')
        return { component: override };
    if (typeof override === 'function')
        return { component: override };
    return { component: override.component, extraProps: override.props };
}
function propsFromNode(props) {
    const reactProps = {};
    for (const [k, v] of Object.entries(props)) {
        if (k === 'className' && typeof v === 'string')
            reactProps.className = v;
        else if (k === 'href' && typeof v === 'string')
            reactProps.href = v;
        else if (typeof v === 'string' || typeof v === 'number' || typeof v === 'boolean')
            reactProps[k] = v;
    }
    return reactProps;
}
export function renderMdAst(nodes, opts) {
    const overrides = opts?.overrides ?? {};
    return nodes.map((n, i) => renderMdAstNode(n, i, opts, overrides));
}
export function renderMdAstNode(n, i, opts, overrides) {
    if (n.type === 'text') {
        return n.content;
    }
    const rawTag = n.tag || 'div';
    const { component, extraProps } = resolveOverride(overrides, rawTag);
    const reactProps = { ...propsFromNode(n.props || {}), ...(extraProps ?? {}), key: i };
    const kids = (n.children || []).map((c, j) => renderMdAstNode(c, j, opts, overrides));
    if (rawTag === 'a' && opts) {
        if (opts.anchorProps)
            Object.assign(reactProps, opts.anchorProps);
        if (opts.anchorOnClick !== undefined) {
            reactProps.onClick = opts.anchorOnClick;
        }
    }
    if (typeof component === 'string') {
        return createElement(component, reactProps, ...kids);
    }
    const Comp = component;
    return _jsx(Comp, { ...reactProps, children: kids });
}
//# sourceMappingURL=mdAst.js.map