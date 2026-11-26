//! CleverScript SSR bake helper: render path(s) at a rev, QA, print body HTML or JSON.
//!
//! Usage:
//!   relay-ssr-bake --repo /opt/relay/repos/paradigm-threat-files.git --rev main --path /
//!   relay-ssr-bake --repo … --rev OID --paths-file paths.txt --out-dir /tmp/bodies
//!
//! Exit 0 only when every path passes QA. Bodies are client-hook HTML fragments.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ! {
    eprintln!(
        "Usage: relay-ssr-bake --repo <bare|checkout> --rev <branch|oid> \\\n\
         \t(--path <url> | --paths-file <file>) [--out-dir <dir>] [--json]"
    );
    std::process::exit(2);
}

fn parse_args() -> (PathBuf, String, Vec<String>, Option<PathBuf>, bool) {
    let mut args = std::env::args().skip(1);
    let mut repo = None;
    let mut rev = None;
    let mut paths = Vec::new();
    let mut out_dir = None;
    let mut json = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--repo" => repo = args.next().map(PathBuf::from),
            "--rev" => rev = args.next(),
            "--path" => {
                if let Some(p) = args.next() {
                    paths.push(p);
                }
            }
            "--paths-file" => {
                let Some(f) = args.next() else { usage() };
                let text = std::fs::read_to_string(&f).unwrap_or_else(|e| {
                    eprintln!("read {f}: {e}");
                    std::process::exit(1);
                });
                for line in text.lines() {
                    let line = line.trim();
                    if !line.is_empty() && !line.starts_with('#') {
                        paths.push(line.to_string());
                    }
                }
            }
            "--out-dir" => out_dir = args.next().map(PathBuf::from),
            "--json" => json = true,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    let (Some(repo), Some(rev)) = (repo, rev) else {
        usage();
    };
    if paths.is_empty() {
        paths.push("/".to_string());
    }
    (repo, rev, paths, out_dir, json)
}

fn body_filename(path: &str) -> String {
    let p = path.trim().trim_matches('/');
    if p.is_empty() || p == "index" || p == "index.md" {
        return "index.body.html".into();
    }
    let p = p.strip_suffix(".md").unwrap_or(p);
    format!("{p}.body.html")
}

fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let (repo, rev, paths, out_dir, as_json) = parse_args();
    if let Some(ref d) = out_dir {
        let _ = std::fs::create_dir_all(d);
    }

    let mut results = Vec::new();
    let mut failed = false;
    for path in &paths {
        match relay_server::git::ssr_render::ssr_render_and_qa(&repo, &rev, path, None) {
            Ok(out) => {
                if let Some(ref d) = out_dir {
                    let name = body_filename(path);
                    let dest = d.join(&name);
                    if let Some(parent) = dest.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = std::fs::write(&dest, &out.body_html) {
                        eprintln!("write {}: {e}", dest.display());
                        failed = true;
                        continue;
                    }
                } else if !as_json {
                    let _ = writeln!(std::io::stdout(), "{}", out.body_html);
                }
                results.push(serde_json::json!({
                    "path": path,
                    "ok": true,
                    "bodyBytes": out.body_html.len(),
                }));
            }
            Err(e) => {
                eprintln!("SSR bake QA failed: {e}");
                failed = true;
                results.push(serde_json::json!({
                    "path": path,
                    "ok": false,
                    "error": e.reason,
                }));
            }
        }
    }

    if as_json {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            serde_json::json!({ "results": results })
        );
    }

    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
