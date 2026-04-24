//! `Accept` parsing for QUERY responses (JSON, XML, CSV; default CSV).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryResponseFormat {
    Json,
    Xml,
    Csv,
}

/// Choose response format from `Accept`. Unsupported or missing → CSV.
pub fn negotiate_query_format(accept_header: Option<&str>) -> QueryResponseFormat {
    let Some(raw) = accept_header.map(str::trim).filter(|s| !s.is_empty()) else {
        return QueryResponseFormat::Csv;
    };
    let mut best: Option<(QueryResponseFormat, f32)> = None;
    for part in raw.split(',') {
        let part = part.trim();
        let (mime_part, q) = if let Some(idx) = part.find(';') {
            let (m, rest) = part.split_at(idx);
            let m = m.trim();
            let mut qv = 1.0_f32;
            for seg in rest[1..].split(';') {
                let seg = seg.trim();
                if let Some(n) = seg
                    .strip_prefix("q=")
                    .and_then(|s| s.trim().parse::<f32>().ok())
                {
                    qv = n;
                }
            }
            (m, qv)
        } else {
            (part, 1.0_f32)
        };
        let mime_part = mime_part.to_ascii_lowercase();
        let fmt = if mime_part == "application/json" || mime_part == "text/json" {
            Some(QueryResponseFormat::Json)
        } else if mime_part == "application/xml" || mime_part == "text/xml" {
            Some(QueryResponseFormat::Xml)
        } else if mime_part == "text/csv" || mime_part == "application/csv" {
            Some(QueryResponseFormat::Csv)
        } else {
            None
        };
        if let Some(f) = fmt {
            let replace = match best {
                None => true,
                Some((_, q_old)) => q > q_old,
            };
            if replace {
                best = Some((f, q));
            }
        }
    }
    best.map(|(f, _)| f).unwrap_or(QueryResponseFormat::Csv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_csv() {
        assert_eq!(negotiate_query_format(None), QueryResponseFormat::Csv);
        assert_eq!(negotiate_query_format(Some("")), QueryResponseFormat::Csv);
    }

    #[test]
    fn json_wins() {
        assert_eq!(
            negotiate_query_format(Some("application/json")),
            QueryResponseFormat::Json
        );
    }

    #[test]
    fn q_values() {
        assert_eq!(
            negotiate_query_format(Some("text/csv;q=0.1, application/json;q=0.9")),
            QueryResponseFormat::Json
        );
    }

    #[test]
    fn unknown_types_default_to_csv() {
        assert_eq!(
            negotiate_query_format(Some("application/octet-stream, text/plain;q=0.5")),
            QueryResponseFormat::Csv
        );
    }

    #[test]
    fn text_xml_maps_to_xml() {
        assert_eq!(
            negotiate_query_format(Some("text/xml")),
            QueryResponseFormat::Xml
        );
    }
}
