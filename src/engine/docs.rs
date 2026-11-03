//! Document conversions.
//!
//! Three different mechanisms, chosen per input so results are predictable:
//!
//! * **Office / web documents** (DOCX, EPUB, Markdown, HTML, TXT) go through the
//!   bundled `pandoc.exe`, which understands document *structure*.
//! * **PDF -> PNG** goes through the bundled `pdftoppm.exe`. PDF is a rendering
//!   format; pandoc cannot read it and no LaTeX engine is bundled (offline).
//! * **CSV / JSON / RTF** are handled natively in Rust. pandoc's `json` reader
//!   is pandoc's own AST (not arbitrary JSON) and it has no CSV/RTF reader, so
//!   routing those through it would silently produce nonsense.

use std::fs;
use std::path::{Path, PathBuf};

use crate::engine::external::{self, CommandSpec, Tool};
use crate::engine::formats::Format;
use crate::engine::mime::InputKind;
use crate::engine::{
    is_cancelled, CancelToken, EngineContext, EngineError, ProgressFn, Result, TaskRequest,
};

/// Refuse to build a Markdown/HTML blob out of a 200 MB CSV.
const MAX_TEXT_INPUT: u64 = 64 * 1024 * 1024;

pub fn run(
    req: &TaskRequest,
    ctx: &EngineContext,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let work = ctx.scratch.task_dir(req.id)?;
    progress(Some(0.02));

    match (req.detected.kind, req.target) {
        (InputKind::Pdf, Format::Png) => rasterize_pdf(req, ctx, &work, token, progress),
        (InputKind::Csv, _) => tabular(req, ctx, &work, token, progress),
        (InputKind::Json, _) => json(req, ctx, &work, token, progress),
        (InputKind::Rtf, Format::Txt | Format::Markdown | Format::Html) => {
            rich_text_to_text(req, &work, token, progress)
        }
        _ => pandoc(req, ctx, &work, token, progress),
    }
}

// -----------------------------------------------------------------------------
//  pandoc
// -----------------------------------------------------------------------------

fn reader_for(kind: InputKind) -> Option<&'static str> {
    Some(match kind {
        InputKind::Docx => "docx",
        InputKind::Epub => "epub",
        InputKind::Markdown => "gfm",
        InputKind::Html => "html",
        // Plain text is valid Markdown, so the Markdown reader is the safest
        // way to get it into pandoc's AST.
        InputKind::Txt => "markdown",
        _ => return None,
    })
}

fn writer_for(target: Format) -> Option<&'static str> {
    Some(match target {
        Format::Docx => "docx",
        Format::Markdown => "gfm",
        Format::Html => "html5",
        Format::Txt => "plain",
        Format::Epub => "epub3",
        _ => return None,
    })
}

fn pandoc(
    req: &TaskRequest,
    ctx: &EngineContext,
    work: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let pandoc = ctx.tools.require(Tool::Pandoc)?;
    let reader = reader_for(req.detected.kind).ok_or_else(|| EngineError::Unsupported {
        what: req.detected.kind.label().into(),
        detail: "has no document reader in this build".into(),
    })?;
    let writer = writer_for(req.target).ok_or_else(|| EngineError::Unsupported {
        what: "document".into(),
        detail: format!("{} is not a document target", req.target.label()),
    })?;

    let ext = req.target.extension().unwrap_or("out");
    let out_path = work.join(format!("result.{ext}"));

    let mut spec = CommandSpec::new(pandoc)
        .args(["-f", reader, "-t", writer])
        .arg(req.source.to_string_lossy().into_owned())
        .args(["-o"])
        .arg(out_path.to_string_lossy().into_owned())
        // No hard wrapping: wrapped text turns into <br> in some writers.
        .args(["--wrap=none"]);

    if matches!(req.target, Format::Html | Format::Epub) {
        spec = spec.args(["--standalone"]);
    }

    progress(None); // pandoc gives us no progress signal
    let outcome = external::run_streaming("pandoc", &spec, token, |_, _| {})?;
    if outcome.code.unwrap_or(1) != 0 {
        return Err(EngineError::Tool {
            tool: "pandoc",
            code: outcome.code,
            message: outcome.stderr,
        });
    }
    if !out_path.is_file() {
        return Err(EngineError::Tool {
            tool: "pandoc",
            code: outcome.code,
            message: "pandoc reported success but produced no output file".into(),
        });
    }
    progress(Some(0.97));
    Ok(vec![out_path])
}

// -----------------------------------------------------------------------------
//  pdftoppm
// -----------------------------------------------------------------------------

fn rasterize_pdf(
    req: &TaskRequest,
    ctx: &EngineContext,
    work: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let pdftoppm = ctx.tools.require(Tool::Pdftoppm)?;
    let prefix = work.join("page");

    let spec = CommandSpec::new(pdftoppm)
        .args(["-png", "-r", "150"])
        .arg(req.source.to_string_lossy().into_owned())
        .arg(prefix.to_string_lossy().into_owned());

    progress(None);
    let outcome = external::run_streaming("pdftoppm", &spec, token, |_, _| {})?;
    if outcome.code.unwrap_or(1) != 0 {
        return Err(EngineError::Tool {
            tool: "pdftoppm",
            code: outcome.code,
            message: outcome.stderr,
        });
    }

    let mut pages = collect_pages(work)?;
    if pages.is_empty() {
        return Err(EngineError::Tool {
            tool: "pdftoppm",
            code: outcome.code,
            message: "no pages were rendered (is the PDF password protected?)".into(),
        });
    }
    // `pdftoppm` numbers pages page-1.png, page-2.png, ... but a single page may
    // also come out as page.png depending on the build.
    pages.sort();
    progress(Some(0.97));
    Ok(pages)
}

fn collect_pages(work: &Path) -> Result<Vec<PathBuf>> {
    let mut pages: Vec<(u32, PathBuf)> = Vec::new();
    for entry in fs::read_dir(work)?.flatten() {
        let path = entry.path();
        let is_png = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("png"))
            .unwrap_or(false);
        if !is_png {
            continue;
        }
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let number = name
            .rsplit('-')
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0);
        pages.push((number, path));
    }
    pages.sort_by_key(|(n, _)| *n);
    Ok(pages.into_iter().map(|(_, p)| p).collect())
}

// -----------------------------------------------------------------------------
//  Native converters: CSV, JSON, RTF
// -----------------------------------------------------------------------------

fn read_text(path: &Path) -> Result<String> {
    let size = fs::metadata(path)?.len();
    if size > MAX_TEXT_INPUT {
        return Err(EngineError::Decode(format!(
            "{} is {} — larger than the {} limit for text conversion",
            path.file_name().unwrap_or_default().to_string_lossy(),
            crate::engine::human_size(size),
            crate::engine::human_size(MAX_TEXT_INPUT)
        )));
    }
    let bytes = fs::read(path)?;
    // Tolerate UTF-8 BOMs and UTF-16 documents exported by Excel.
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return Ok(String::from_utf16_lossy(&units));
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn tabular(
    req: &TaskRequest,
    ctx: &EngineContext,
    work: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let text = read_text(&req.source)?;
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }
    let delimiter = if req
        .source
        .extension()
        .map(|e| e.eq_ignore_ascii_case("tsv"))
        .unwrap_or(false)
    {
        '\t'
    } else {
        ','
    };
    let rows = parse_delimited(&text, delimiter);
    if rows.is_empty() {
        return Err(EngineError::Decode("the CSV file has no rows".into()));
    }
    progress(Some(0.4));

    let rendered = render_table(&rows, req.target);
    let staged = write_staged(work, req.target, &rendered)?;
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }
    finish_document(req, ctx, work, staged, token, progress)
}

fn json(
    req: &TaskRequest,
    ctx: &EngineContext,
    work: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let text = read_text(&req.source)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| EngineError::Decode(format!("invalid JSON: {e}")))?;
    let pretty = serde_json::to_string_pretty(&value)
        .map_err(|e| EngineError::Encode(format!("JSON: {e}")))?;
    progress(Some(0.4));
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }

    let rendered = match req.target {
        Format::Html => format!(
            "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title></head>\
             <body><pre>{}</pre></body></html>\n",
            html_escape(&req.stem()),
            html_escape(&pretty)
        ),
        Format::Markdown | Format::Docx | Format::Epub => {
            format!("```json\n{pretty}\n```\n")
        }
        _ => format!("{pretty}\n"),
    };

    let staged = write_staged(work, req.target, &rendered)?;
    finish_document(req, ctx, work, staged, token, progress)
}

fn rich_text_to_text(
    req: &TaskRequest,
    work: &Path,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    let text = read_text(&req.source)?;
    if is_cancelled(token) {
        return Err(EngineError::Cancelled);
    }
    let plain = rtf_to_text(&text);
    progress(Some(0.6));
    let rendered = match req.target {
        Format::Html => format!(
            "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{}</title></head>\
             <body><p>{}</p></body></html>\n",
            html_escape(&req.stem()),
            html_escape(&plain)
                .replace("\n\n", "</p><p>")
                .replace('\n', "<br>")
        ),
        _ => plain,
    };
    // RTF only offers text targets, so no further tool is required.
    let out = work.join(format!(
        "result.{}",
        req.target.extension().unwrap_or("txt")
    ));
    fs::write(&out, rendered)?;
    progress(Some(0.97));
    Ok(vec![out])
}

/// Write `rendered` into the scratch area, and return its path. For targets
/// that only pandoc can write, stage an intermediate Markdown file instead.
fn write_staged(work: &Path, target: Format, rendered: &str) -> Result<PathBuf> {
    let (path, content) = match target {
        Format::Docx | Format::Epub => (work.join("staged.md"), rendered.to_string()),
        Format::Txt => (work.join("staged.txt"), rendered.to_string()),
        Format::Html => (work.join("staged.html"), rendered.to_string()),
        _ => (work.join("staged.md"), rendered.to_string()),
    };
    fs::write(&path, content)?;
    Ok(path)
}

/// Either hand the staged file back (when the format is already final) or run
/// it through pandoc for DOCX/EPUB.
fn finish_document(
    req: &TaskRequest,
    ctx: &EngineContext,
    work: &Path,
    staged: PathBuf,
    token: &CancelToken,
    mut progress: ProgressFn<'_>,
) -> Result<Vec<PathBuf>> {
    if matches!(req.target, Format::Docx | Format::Epub) {
        let pandoc = ctx.tools.require(Tool::Pandoc)?;
        let out = work.join(format!(
            "result.{}",
            req.target.extension().unwrap_or("docx")
        ));
        let spec = CommandSpec::new(pandoc)
            .args(["-f", "gfm", "-t", writer_for(req.target).unwrap_or("docx")])
            .arg(staged.to_string_lossy().into_owned())
            .args(["-o"])
            .arg(out.to_string_lossy().into_owned())
            .args(["--wrap=none"]);
        let outcome = external::run_streaming("pandoc", &spec, token, |_, _| {})?;
        if outcome.code.unwrap_or(1) != 0 {
            return Err(EngineError::Tool {
                tool: "pandoc",
                code: outcome.code,
                message: outcome.stderr,
            });
        }
        progress(Some(0.97));
        return Ok(vec![out]);
    }

    let ext = req.target.extension().unwrap_or("txt");
    let out = work.join(format!("result.{ext}"));
    fs::rename(&staged, &out)?;
    progress(Some(0.97));
    Ok(vec![out])
}

// -----------------------------------------------------------------------------
//  Parsers / renderers (pure, unit tested)
// -----------------------------------------------------------------------------

/// Minimal RFC 4180 parser: quoted fields, `""` escapes, CRLF or LF.
pub fn parse_delimited(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => field.push(other),
            }
            continue;
        }
        match c {
            '"' if field.trim().is_empty() => {
                field.clear();
                in_quotes = true;
            }
            c if c == delimiter => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            other => field.push(other),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Drop rows that are entirely empty (trailing newline at EOF).
    rows.retain(|r| !(r.len() == 1 && r[0].trim().is_empty()));
    rows
}

fn render_table(rows: &[Vec<String>], target: Format) -> String {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    fn cell(row: &[String], i: usize) -> &str {
        row.get(i).map(String::as_str).unwrap_or("")
    }

    match target {
        Format::Html => {
            let mut out = String::from(
                "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
                <style>table{border-collapse:collapse}td,th{border:1px solid #ccc;padding:4px 8px}\
                th{background:#f0f0f0}</style></head><body><table>\n",
            );
            for (index, row) in rows.iter().enumerate() {
                out.push_str("<tr>");
                let tag = if index == 0 { "th" } else { "td" };
                for i in 0..columns {
                    out.push_str(&format!("<{tag}>{}</{tag}>", html_escape(cell(row, i))));
                }
                out.push_str("</tr>\n");
            }
            out.push_str("</table></body></html>\n");
            out
        }
        Format::Markdown | Format::Docx | Format::Epub => {
            let mut out = String::new();
            if let Some(header) = rows.first() {
                out.push('|');
                for i in 0..columns {
                    out.push_str(&format!(" {} |", md_escape(cell(header, i))));
                }
                out.push('\n');
                out.push('|');
                for _ in 0..columns {
                    out.push_str(" --- |");
                }
                out.push('\n');
                for row in &rows[1..] {
                    out.push('|');
                    for i in 0..columns {
                        out.push_str(&format!(" {} |", md_escape(cell(row, i))));
                    }
                    out.push('\n');
                }
            }
            out
        }
        _ => {
            // Plain text: align columns with spaces.
            let mut widths = vec![0usize; columns];
            for row in rows {
                for i in 0..columns {
                    widths[i] = widths[i].max(cell(row, i).chars().count());
                }
            }
            let mut out = String::new();
            for row in rows {
                let mut line = String::new();
                for i in 0..columns {
                    line.push_str(cell(row, i));
                    if i + 1 < columns {
                        let pad = widths[i] - cell(row, i).chars().count() + 2;
                        line.push_str(&" ".repeat(pad));
                    }
                }
                out.push_str(line.trim_end());
                out.push('\n');
            }
            out
        }
    }
}

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// Best-effort RTF -> text. Strips control words, drops destination groups that
/// contain no user text (font/colour tables, pictures, headers), decodes
/// `\uN` and `\'hh` escapes and maps `\par`/`\line` to newlines.
pub fn rtf_to_text(src: &str) -> String {
    const SKIP_DESTINATIONS: &[&str] = &[
        "fonttbl",
        "colortbl",
        "stylesheet",
        "info",
        "pict",
        "object",
        "header",
        "footer",
        "headerl",
        "headerr",
        "footerl",
        "footerr",
        "listtable",
        "listoverridetable",
        "xmlnstbl",
        "generator",
        "themedata",
        "colorschememapping",
    ];

    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut depth: usize = 0;
    let mut skip_depth: Option<usize> = None;
    let mut i = 0usize;
    let mut pending_high_surrogate: Option<u32> = None;

    let push_char = |out: &mut String, c: char| {
        if c == '\u{0}' {
            return;
        }
        out.push(c);
    };

    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => {
                depth += 1;
                i += 1;
            }
            '}' => {
                if let Some(sd) = skip_depth {
                    if depth <= sd {
                        skip_depth = None;
                    }
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    break;
                }
                let next = chars[i];
                match next {
                    '\\' | '{' | '}' => {
                        if skip_depth.is_none() {
                            push_char(&mut out, next);
                        }
                        i += 1;
                    }
                    '\'' => {
                        // \'hh -> a byte in the document's code page.
                        if i + 2 < chars.len() {
                            let hex: String = chars[i + 1..i + 3].iter().collect();
                            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                                if skip_depth.is_none() {
                                    push_char(&mut out, byte as char);
                                }
                                i += 3;
                                continue;
                            }
                        }
                        i += 1;
                    }
                    '*' => {
                        // Ignorable destination: skip its whole group.
                        skip_depth = Some(depth);
                        i += 1;
                    }
                    c if c.is_ascii_alphabetic() => {
                        let start = i;
                        while i < chars.len() && chars[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        let word: String = chars[start..i].iter().collect();
                        let num_start = i;
                        if i < chars.len() && (chars[i] == '-' || chars[i].is_ascii_digit()) {
                            i += 1;
                            while i < chars.len() && chars[i].is_ascii_digit() {
                                i += 1;
                            }
                        }
                        let number: Option<i64> =
                            chars[num_start..i].iter().collect::<String>().parse().ok();
                        if i < chars.len() && chars[i] == ' ' {
                            i += 1;
                        }

                        if SKIP_DESTINATIONS.contains(&word.as_str()) {
                            skip_depth = Some(depth);
                            continue;
                        }
                        if skip_depth.is_some() {
                            continue;
                        }
                        match word.as_str() {
                            "par" | "line" | "sect" | "page" => out.push('\n'),
                            "tab" => out.push('\t'),
                            "emdash" => out.push('—'),
                            "endash" => out.push('–'),
                            "lquote" => out.push('‘'),
                            "rquote" => out.push('’'),
                            "ldblquote" => out.push('“'),
                            "rdblquote" => out.push('”'),
                            "bullet" => out.push('•'),
                            "u" => {
                                if let Some(n) = number {
                                    let code = (n as u32) & 0xFFFF;
                                    if (0xD800..0xDC00).contains(&code) {
                                        pending_high_surrogate = Some(code);
                                    } else if (0xDC00..0xE000).contains(&code) {
                                        if let Some(high) = pending_high_surrogate.take() {
                                            let cp =
                                                0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00);
                                            if let Some(ch) = char::from_u32(cp) {
                                                push_char(&mut out, ch);
                                            }
                                        }
                                    } else if let Some(ch) = char::from_u32(code) {
                                        push_char(&mut out, ch);
                                    }
                                    // The fallback character follows as plain
                                    // text; skip it.
                                    if i < chars.len() && chars[i] == '?' {
                                        i += 1;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        if skip_depth.is_none() {
                            push_char(&mut out, next);
                        }
                        i += 1;
                    }
                }
            }
            '\r' => i += 1,
            '\n' => i += 1,
            _ => {
                if skip_depth.is_none() {
                    out.push(c);
                }
                i += 1;
            }
        }
    }

    // Collapse the runs of blank lines RTF loves to produce.
    let mut cleaned = String::with_capacity(out.len());
    let mut blank_run = 0;
    for line in out.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        cleaned.push_str(trimmed);
        cleaned.push('\n');
    }
    cleaned.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_handles_quotes_and_commas() {
        let rows = parse_delimited("name,note\r\n\"Smith, John\",\"said \"\"hi\"\"\"\r\n", ',');
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0], "Smith, John");
        assert_eq!(rows[1][1], "said \"hi\"");
    }

    #[test]
    fn csv_ignores_trailing_newline() {
        let rows = parse_delimited("a,b\n1,2\n", ',');
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec!["a", "b"]);
    }

    #[test]
    fn tsv_uses_tabs() {
        let rows = parse_delimited("a\tb\n1\t2", '\t');
        assert_eq!(rows[1], vec!["1", "2"]);
    }

    #[test]
    fn markdown_table_is_generated_with_escaped_pipes() {
        let rows = vec![
            vec!["col|one".to_string(), "two".to_string()],
            vec!["1".to_string(), "2".to_string()],
        ];
        let md = render_table(&rows, Format::Markdown);
        assert!(md.contains("| col\\|one | two |"), "{md}");
        assert!(md.contains("| --- | --- |"), "{md}");
    }

    #[test]
    fn html_table_escapes_content() {
        let rows = vec![vec!["<script>".to_string()], vec!["x".to_string()]];
        let html = render_table(&rows, Format::Html);
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<script><"));
    }

    #[test]
    fn plain_text_table_is_padded() {
        let rows = vec![
            vec!["name".to_string(), "age".to_string()],
            vec!["Ada".to_string(), "36".to_string()],
        ];
        let text = render_table(&rows, Format::Txt);
        let first = text.lines().next().unwrap();
        assert!(first.starts_with("name"), "{text}");
        assert!(first.contains("age"), "{text}");
    }

    #[test]
    fn rtf_is_flattened_to_text() {
        let rtf = r#"{\rtf1\ansi{\fonttbl{\f0 Segoe UI;}}{\colortbl;\red0\green0\blue0;}
\f0\fs20 Hello \b world\b0!\par Second line.\par}"#;
        let text = rtf_to_text(rtf);
        assert!(text.contains("Hello world!"), "{text}");
        assert!(text.contains("Second line."), "{text}");
        assert!(!text.contains("Segoe UI"), "font table leaked: {text}");
    }

    #[test]
    fn rtf_unicode_escapes_decode() {
        let text = rtf_to_text(r#"{\rtf1 caf\'e9 \u8364?}"#);
        assert!(text.contains("café"), "{text}");
        assert!(text.contains('€'), "{text}");
    }

    #[test]
    fn pandoc_readers_and_writers_exist_for_every_document_target() {
        for kind in [
            InputKind::Docx,
            InputKind::Epub,
            InputKind::Markdown,
            InputKind::Html,
            InputKind::Txt,
        ] {
            assert!(reader_for(kind).is_some(), "{kind:?} has no reader");
        }
        for target in [
            Format::Docx,
            Format::Markdown,
            Format::Html,
            Format::Txt,
            Format::Epub,
        ] {
            assert!(writer_for(target).is_some(), "{target:?} has no writer");
        }
    }
}
