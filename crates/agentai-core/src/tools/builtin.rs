//! The built-in tools: list, read and write files inside one folder the person chose, and read
//! web pages.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::error::clip;
use crate::types::ToolSpec;
use crate::{Error, Result};

const MAX_READ: u64 = 2 << 20;
const MAX_WRITE: usize = 10 << 20;

pub fn specs(files: bool, web: bool) -> Vec<ToolSpec> {
    let mut v = Vec::new();
    let path = json!({"type": "string", "description": "Path relative to the working folder ('.' for the folder itself)"});
    if files {
        v.push(ToolSpec {
            name: "list_folder".into(),
            description: "List the files and folders in a folder of the working folder.".into(),
            schema: json!({"type": "object", "properties": {"path": path}, "required": ["path"], "additionalProperties": false}),
        });
        v.push(ToolSpec {
            name: "read_file".into(),
            description: "Read a text file in the working folder (up to 2 MB).".into(),
            schema: json!({"type": "object", "properties": {"path": path}, "required": ["path"], "additionalProperties": false}),
        });
        v.push(ToolSpec {
            name: "write_file".into(),
            description: "Create or replace a text file in the working folder. The person is asked first.".into(),
            schema: json!({"type": "object", "properties": {"path": path, "content": {"type": "string"}}, "required": ["path", "content"], "additionalProperties": false}),
        });
    }
    if web {
        v.push(ToolSpec {
            name: "fetch_url".into(),
            description: "Read a web page (http or https) and return its text.".into(),
            schema: json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"], "additionalProperties": false}),
        });
    }
    v
}

/// `Some(ask first)` for a built-in tool, `None` for any other name.
pub fn needs_approval(name: &str) -> Option<bool> {
    match name {
        "list_folder" | "read_file" | "fetch_url" => Some(false),
        "write_file" => Some(true),
        _ => None,
    }
}

pub fn call(name: &str, input: &Value, folder: Option<&Path>, web: bool) -> Result<String> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).ok_or_else(|| Error::Tool(format!("missing \"{k}\"")));
    match name {
        "fetch_url" if web => fetch(s("url")?),
        "list_folder" | "read_file" | "write_file" => {
            let root = folder.ok_or_else(|| Error::Tool("no working folder is chosen for this agent".into()))?;
            let p = resolve(root, s("path")?)?;
            match name {
                "list_folder" => list(&p),
                "read_file" => read(&p),
                _ => write(root, &p, s("content")?),
            }
        }
        _ => Err(Error::Tool(format!("the tool {name} is turned off"))),
    }
}

/// `rel` inside `root`, refusing anything that would leave it.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = Path::new(rel.trim());
    let rel = rel.strip_prefix(root).unwrap_or(rel);
    let mut out = PathBuf::new();
    for c in rel.components() {
        match c {
            Component::Normal(x) => out.push(x),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(Error::Tool("that path is outside the working folder".into()));
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(Error::Tool("use a path inside the working folder".into())),
        }
    }
    let full = root.join(out);
    // A link inside the folder that points outside it is refused too.
    if let (Ok(r), Ok(f)) = (root.canonicalize(), full.canonicalize())
        && !f.starts_with(&r)
    {
        return Err(Error::Tool("that path is outside the working folder".into()));
    }
    Ok(full)
}

fn list(p: &Path) -> Result<String> {
    let mut items: Vec<String> = std::fs::read_dir(p)?
        .filter_map(|e| e.ok())
        .take(2000)
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            match e.metadata() {
                Ok(m) if m.is_dir() => format!("{name}/"),
                Ok(m) => format!("{name} ({} bytes)", m.len()),
                Err(_) => name,
            }
        })
        .collect();
    items.sort();
    Ok(if items.is_empty() { "(empty folder)".into() } else { items.join("\n") })
}

fn read(p: &Path) -> Result<String> {
    let len = std::fs::metadata(p)?.len();
    if len > MAX_READ {
        return Err(Error::Tool(format!("the file is {len} bytes; the limit is {MAX_READ}")));
    }
    let bytes = std::fs::read(p)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn write(root: &Path, p: &Path, content: &str) -> Result<String> {
    if content.len() > MAX_WRITE {
        return Err(Error::Tool("the content is too large".into()));
    }
    if let Some(parent) = p.parent()
        && parent.starts_with(root)
    {
        std::fs::create_dir_all(parent)?;
    }
    // Written beside the target first, then moved over it, so a failed write never leaves half a file.
    let tmp = p.with_extension("agentai-tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, p)?;
    Ok(format!("saved {} ({} bytes)", p.display(), content.len()))
}

fn fetch(url: &str) -> Result<String> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Error::Tool("only http and https addresses can be read".into()));
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("AgentAI/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let resp = agent.get(url).call()?;
    let status = resp.status().as_u16();
    let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let body = resp.into_body().with_config().limit(5 << 20).read_to_vec()?;
    if !(200..300).contains(&status) {
        return Err(Error::Tool(format!("the page answered {status}")));
    }
    let text = String::from_utf8_lossy(&body);
    let out = if ctype.contains("html") || text.trim_start().starts_with('<') { html_text(&text) } else { text.into_owned() };
    Ok(clip(&out, super::MAX_RESULT))
}

/// The readable text of an HTML page: scripts, styles and tags removed, entities decoded, blank
/// lines collapsed.
pub fn html_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 3);
    let lower = html.to_ascii_lowercase(); // same byte offsets as `html`
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'<') {
            let rest = lower.get(i..).unwrap_or("");
            let skip_to = ["script", "style", "noscript", "svg", "head"].iter().find_map(|t| {
                if rest.starts_with(&format!("<{t}")) {
                    let close = format!("</{t}>");
                    Some(rest.find(&close).map(|e| i + e + close.len()).unwrap_or(bytes.len()))
                } else {
                    None
                }
            });
            let end = skip_to.unwrap_or_else(|| rest.find('>').map(|e| i + e + 1).unwrap_or(bytes.len()));
            let tag = rest.get(1..).unwrap_or("");
            if ["p", "div", "br", "li", "h1", "h2", "h3", "h4", "tr", "/p", "/div", "/li", "/h", "section", "article"]
                .iter()
                .any(|t| tag.starts_with(t))
            {
                out.push('\n');
            }
            i = end;
            continue;
        }
        let next = lower.get(i..).and_then(|r| r.find('<')).map(|n| i + n).unwrap_or(bytes.len());
        if let Some(chunk) = html.get(i..next) {
            out.push_str(&decode_entities(chunk));
        }
        if next <= i {
            break;
        }
        i = next;
    }
    let mut cleaned = String::new();
    let mut blank = 0;
    for line in out.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        cleaned.push_str(&l);
        cleaned.push('\n');
    }
    cleaned.trim().to_string()
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_folder() {
        let dir = std::env::temp_dir().join(format!("agentai-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(resolve(&dir, "../etc/passwd").is_err());
        assert!(resolve(&dir, "/etc/passwd").is_err());
        assert!(resolve(&dir, "a/../../x").is_err());
        assert_eq!(resolve(&dir, "./a/b.txt").unwrap(), dir.join("a/b.txt"));
        let out = call("write_file", &json!({"path": "sub/ملف.txt", "content": "نص"}), Some(&dir), false).unwrap();
        assert!(out.contains("saved"));
        assert_eq!(call("read_file", &json!({"path": "sub/ملف.txt"}), Some(&dir), false).unwrap(), "نص");
        assert!(call("list_folder", &json!({"path": "."}), Some(&dir), false).unwrap().contains("sub/"));
        assert!(call("read_file", &json!({}), Some(&dir), false).is_err());
        assert!(call("read_file", &json!({"path": "x"}), None, false).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn web_is_off_unless_allowed() {
        assert!(call("fetch_url", &json!({"url": "https://example.com"}), None, false).is_err());
        assert!(call("fetch_url", &json!({"url": "file:///etc/passwd"}), None, true).is_err());
    }

    #[test]
    fn html_becomes_text() {
        let t = html_text(
            "<html><head><title>x</title><style>a{}</style></head><body><h1>عنوان</h1><p>Hello &amp; <b>bye</b></p><script>evil()</script></body></html>",
        );
        assert_eq!(t, "عنوان\n\nHello & bye");
        assert_eq!(html_text("<<<>"), "");
        assert_eq!(html_text("plain"), "plain");
    }
}
