//! Convert noisy page HTML into compact Markdown before it reaches an LLM.
//! Scripts, styles, navigation chrome and vector graphics are dropped.

use htmd::HtmlToMarkdown;

const SKIP_TAGS: &[&str] = &[
    "script", "style", "nav", "header", "footer", "svg", "aside", "noscript", "iframe", "form", "button", "head",
];

/// Strip noise tags and convert the remaining DOM to Markdown with at most one
/// blank line between blocks.
pub fn html_to_markdown(html: &str) -> String {
    let converter = HtmlToMarkdown::builder().skip_tags(SKIP_TAGS.to_vec()).build();
    let md = converter.convert(html).unwrap_or_default();
    let mut out = String::with_capacity(md.len());
    let mut blank_run = 0;
    for line in md.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}
