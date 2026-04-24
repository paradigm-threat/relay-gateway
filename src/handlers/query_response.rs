//! Serialize QUERY result rows as JSON, XML, or CSV.

use crate::handlers::accept::QueryResponseFormat;
use serde_json::Value;

pub fn serialize_query_results(
    format: QueryResponseFormat,
    rows: &[Value],
) -> (String, &'static str) {
    match format {
        QueryResponseFormat::Json => (
            serde_json::to_string(&serde_json::json!({ "results": rows }))
                .unwrap_or_else(|_| "{}".into()),
            "application/json; charset=utf-8",
        ),
        QueryResponseFormat::Xml => (rows_to_xml(rows), "application/xml; charset=utf-8"),
        QueryResponseFormat::Csv => (rows_to_csv(rows), "text/csv; charset=utf-8"),
    }
}

fn rows_to_csv(rows: &[Value]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let keys = csv_header_keys(rows);
    let mut lines = Vec::new();
    lines.push(csv_line(
        &keys.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    ));
    for row in rows {
        let cells: Vec<String> = keys
            .iter()
            .map(|k| cell_string(row.get(k).unwrap_or(&Value::Null)))
            .collect();
        lines.push(csv_line(
            &cells.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ));
    }
    lines.join("\n")
}

fn csv_line(cells: &[&str]) -> String {
    cells
        .iter()
        .map(|c| {
            let needs_quote =
                c.contains(',') || c.contains('"') || c.contains('\n') || c.contains('\r');
            if needs_quote {
                format!("\"{}\"", c.replace('"', "\"\""))
            } else {
                (*c).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_header_keys(rows: &[Value]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let Some(first) = rows.first().and_then(|v| v.as_object()) else {
        return keys;
    };
    for k in first.keys() {
        keys.push(k.clone());
    }
    keys.sort();
    keys
}

fn cell_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

fn rows_to_xml(rows: &[Value]) -> String {
    let mut s = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    s.push_str("\n<results>\n");
    for row in rows {
        s.push_str("  <row>\n");
        if let Some(obj) = row.as_object() {
            let mut keys: Vec<_> = obj.keys().cloned().collect();
            keys.sort();
            for k in keys {
                if let Some(v) = obj.get(&k) {
                    let tag = xml_safe_tag_name(&k);
                    s.push_str("    <");
                    s.push_str(&tag);
                    s.push('>');
                    s.push_str(&escape_xml_text(&cell_string(v)));
                    s.push_str("</");
                    s.push_str(&tag);
                    s.push_str(">\n");
                }
            }
        }
        s.push_str("  </row>\n");
    }
    s.push_str("</results>\n");
    s
}

fn xml_safe_tag_name(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        let ok = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || (ch == '.' && i > 0);
        if ok {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('f');
    }
    if let Some(c) = out.chars().next() {
        if c.is_ascii_digit() {
            out.insert(0, '_');
        }
    }
    out
}

fn escape_xml_text(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect::<Vec<_>>(),
            '>' => "&gt;".chars().collect::<Vec<_>>(),
            '"' => "&quot;".chars().collect::<Vec<_>>(),
            _ => vec![c],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::accept::QueryResponseFormat;
    use serde_json::json;

    #[test]
    fn serialize_json_wraps_results() {
        let rows = vec![json!({ "a": 1 })];
        let (s, ct) = serialize_query_results(QueryResponseFormat::Json, &rows);
        assert!(ct.contains("json"));
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["results"][0]["a"], 1);
    }

    #[test]
    fn csv_quotes_commas_and_escapes_quotes() {
        let rows = vec![json!({ "t": "a,b", "u": "say \"hi\"" })];
        let (s, ct) = serialize_query_results(QueryResponseFormat::Csv, &rows);
        assert!(ct.contains("csv"));
        assert!(s.contains("\"a,b\""));
        assert!(s.contains("\"say \"\"hi\"\"\""));
    }

    #[test]
    fn xml_escapes_special_chars_in_text() {
        let rows = vec![json!({ "t": "a&b<c>" })];
        let (s, ct) = serialize_query_results(QueryResponseFormat::Xml, &rows);
        assert!(ct.contains("xml"));
        assert!(s.contains("a&amp;b&lt;c&gt;"));
        assert!(s.contains("<results>"));
    }

    #[test]
    fn empty_csv_is_empty_string() {
        let (s, _) = serialize_query_results(QueryResponseFormat::Csv, &[]);
        assert!(s.is_empty());
    }
}
