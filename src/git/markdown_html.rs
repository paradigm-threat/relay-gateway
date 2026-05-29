//! Markdown → static HTML for GET hooks (`@relay/md` / `markdownHtmlFromGetContext`).

use md2ast::{parse as md2ast_parse, Node, TranspileOptions};
use serde_json::Value;

/// HTML tag allowlist for md2ast (standard blocks + paradigm-threat inline HTML).
pub fn default_markdown_html_allowed_tags() -> Vec<String> {
    let mut tags: Vec<String> = [
        "a", "abbr", "article", "b", "blockquote", "br", "cite", "code", "del", "div", "em",
        "figcaption", "figure", "h1", "h2", "h3", "h4", "h5", "h6", "hr", "i", "iframe", "img",
        "ins", "li", "mark", "nav", "ol", "p", "pre", "q", "s", "section", "small", "span",
        "strong", "sub", "sup", "table", "tbody", "td", "th", "thead", "time", "tr", "u", "ul",
        "video", "math", "annotation", "semantics", "mrow", "mi", "mo", "mn", "msup", "msub",
        "mfrac", "mtext",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    tags.sort();
    tags.dedup();
    tags
}

pub fn is_markdown_content_type(ct: &str) -> bool {
    let ct = ct.to_ascii_lowercase();
    ct.contains("markdown") || ct == "text/md" || ct == "text/x-markdown"
}

pub fn is_markdown_path(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".md")
}

/// Strip YAML frontmatter (`---` … `---`) from the start of markdown.
pub fn strip_yaml_frontmatter(markdown: &str) -> &str {
    if !markdown.starts_with("---") {
        return markdown;
    }
    let after_open = &markdown[3..];
    let Some(idx) = after_open.find("\n---") else {
        return markdown;
    };
    let rest = &markdown[3 + idx + 4..];
    rest.strip_prefix('\n').unwrap_or(rest)
}

fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_html_attr(s: &str) -> String {
    escape_html_text(s)
}

fn json_prop_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn render_nodes_html(nodes: &[Node], out: &mut String) {
    for n in nodes {
        render_node_html(n, out);
    }
}

fn html_attr_name(key: &str) -> &str {
    if key == "className" {
        "class"
    } else {
        key
    }
}

fn should_skip_html_attr(key: &str, value: &str) -> bool {
    if key == "title" {
        let t = value.trim();
        return t.starts_with('{') && t.ends_with('}');
    }
    false
}

fn render_node_html(node: &Node, out: &mut String) {
    match node {
        Node::Text { content } => {
            out.push_str(&escape_html_text(content));
        }
        Node::Element { tag, props, children } => {
            let tag = tag.as_str();
            if tag.is_empty() {
                render_nodes_html(children, out);
                return;
            }
            out.push('<');
            out.push_str(tag);
            for (k, v) in props {
                if let Some(s) = json_prop_string(v) {
                    if should_skip_html_attr(k, &s) {
                        continue;
                    }
                    if !s.is_empty() || k == "alt" {
                        out.push(' ');
                        out.push_str(html_attr_name(k));
                        out.push_str("=\"");
                        out.push_str(&escape_html_attr(&s));
                        out.push('"');
                    }
                }
            }
            if children.is_empty() && is_void_html_tag(tag) {
                out.push_str(" />");
            } else {
                out.push('>');
                render_nodes_html(children, out);
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
}

fn is_void_html_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr"
    )
}

/// Parse markdown and emit a semantic HTML fragment wrapped in `<article class="relay-md-html">`.
pub fn markdown_to_html_fragment(markdown: &str) -> Result<String, String> {
    let body_md = strip_yaml_frontmatter(markdown);
    let options = TranspileOptions {
        allowed_tags: default_markdown_html_allowed_tags(),
    };
    let nodes = md2ast_parse(body_md, &options);
    let mut body = String::new();
    render_nodes_html(&nodes, &mut body);
    Ok(format!(
        "<article class=\"relay-md-html mdx-content\" data-relay-md-html=\"true\">{body}</article>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_to_html_basic() {
        let html = markdown_to_html_fragment("# Title\n\nHello **world**.").unwrap();
        assert!(html.contains("<article"));
        assert!(html.contains("<h1>"));
        assert!(html.contains("Title"));
        assert!(html.contains("Hello"));
    }

    #[test]
    fn markdown_image_has_src() {
        let html = markdown_to_html_fragment("![alt](/img.jpg)").unwrap();
        assert!(html.contains("src=\"/img.jpg\""));
    }

    #[test]
    fn markdown_strips_yaml_frontmatter() {
        let md = "---\ntitle: Welcome\ndescription: ignored\n---\n# Welcome\n\nBody.";
        let html = markdown_to_html_fragment(md).unwrap();
        assert!(!html.contains("title: Welcome"));
        assert!(!html.contains("description: ignored"));
        assert!(html.contains("<h1>"));
        assert!(html.contains("Welcome"));
        assert!(html.contains("Body"));
    }

    #[test]
    fn markdown_image_json_title_becomes_class() {
        let md = r#"![alt](/influence/conspiracy/embedding.jpg?w=360 '{"className":"sm:float-right clear-both"}')"#;
        let html = markdown_to_html_fragment(md).unwrap();
        assert!(html.contains("class=\"sm:float-right clear-both\""), "html: {html}");
        assert!(!html.contains("title=\"{"));
    }
}
