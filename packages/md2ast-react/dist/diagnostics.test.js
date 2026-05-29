import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validateMarkdownSource, diagnoseMarkdownError, MarkdownCompileError, } from '../dist/diagnostics.js';
const allowed = ['p', 'AutoIndex', 'ChatRoom'];
test('unknown tag reports line and excerpt', () => {
    const src = '# Title\n\n<UnknownWidget />\n\nMore text.';
    assert.throws(() => validateMarkdownSource(src, allowed), (e) => {
        assert.ok(e instanceof MarkdownCompileError);
        assert.equal(e.diagnostics.kind, 'unknown_tag');
        assert.equal(e.diagnostics.line, 3);
        assert.ok(e.diagnostics.codeFrame?.includes('UnknownWidget'));
        return true;
    });
});
test('mdx jsx comment is rejected with line', () => {
    const src = 'Hello\n{/* comment */}\n';
    assert.throws(() => validateMarkdownSource(src, allowed), (e) => {
        assert.ok(e instanceof MarkdownCompileError);
        assert.equal(e.diagnostics.kind, 'mdx_syntax');
        assert.equal(e.diagnostics.line, 2);
        return true;
    });
});
test('diagnoseMarkdownError builds frame from line in message', () => {
    const src = 'a\nb\nc\n';
    const d = diagnoseMarkdownError(new Error('parse failed at line 2'), src);
    assert.equal(d.line, 2);
    assert.ok(d.codeFrame?.includes('b'));
});
//# sourceMappingURL=diagnostics.test.js.map