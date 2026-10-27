//! Static syntax check for every inline script this app serves.
//!
//! Substring assertions cannot tell a live page from a dead one: one raw
//! newline inside a JavaScript string literal leaves the whole renderer inert
//! while every `contains()` test still passes (the 3c-6 form-card breakage).
//! This lexer walks a script the way an engine does — strings, templates,
//! comments, and regex literals — and rejects an unterminated literal, an
//! unterminated comment or regex, and unbalanced brackets.
//!
//! It is deliberately a syntax *shape* check, not a parser: it catches the
//! failures that silently kill a page, and says nothing about semantics.

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Code,
    Single,
    Double,
    Template,
    LineComment,
    BlockComment,
    Regex,
    RegexClass,
}

/// Why `src` would not parse as a script body, if it would not.
fn js_syntax_error(src: &str) -> Result<(), String> {
    let cs: Vec<char> = src.chars().collect();
    let mut mode = Mode::Code;
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut prev = '\0';
    let mut word = String::new();
    let mut line = 1usize;
    let mut lit_line = 1usize;
    let mut escaped = false;
    let mut i = 0usize;
    while i < cs.len() {
        let c = cs[i];
        if escaped {
            // The escaped character is consumed literally in every mode that
            // honours backslash escapes; it can never open or close anything.
            escaped = false;
            if c == '\n' {
                line += 1;
            }
            i += 1;
            continue;
        }
        let next = cs.get(i + 1).copied();
        match mode {
            Mode::Code => {
                if c == '\'' {
                    mode = Mode::Single;
                    lit_line = line;
                } else if c == '"' {
                    mode = Mode::Double;
                    lit_line = line;
                } else if c == '`' {
                    mode = Mode::Template;
                    lit_line = line;
                } else if c == '/' && next == Some('/') {
                    mode = Mode::LineComment;
                    i += 1;
                } else if c == '/' && next == Some('*') {
                    mode = Mode::BlockComment;
                    i += 1;
                } else if c == '/' {
                    if regex_can_start(prev, &word) {
                        mode = Mode::Regex;
                        lit_line = line;
                    } else {
                        prev = '/';
                        word.clear();
                    }
                } else if matches!(c, '(' | '[' | '{') {
                    stack.push((c, line));
                    prev = c;
                    word.clear();
                } else if matches!(c, ')' | ']' | '}') {
                    let want = match c {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    match stack.pop() {
                        Some((open, _)) if open == want => {}
                        Some((open, at)) => {
                            return Err(format!(
                                "line {line}: `{c}` closes `{open}` opened on line {at}"
                            ));
                        }
                        None => return Err(format!("line {line}: unmatched `{c}`")),
                    }
                    prev = c;
                    word.clear();
                } else if is_ident(c) {
                    word.push(c);
                    prev = c;
                } else if !c.is_whitespace() {
                    prev = c;
                    word.clear();
                }
            }
            Mode::Single | Mode::Double => {
                let quote = if mode == Mode::Single { '\'' } else { '"' };
                if c == '\\' {
                    escaped = true;
                } else if c == '\n' || c == '\r' {
                    return Err(format!(
                        "line {line}: newline inside string literal opened on line {lit_line}"
                    ));
                } else if c == quote {
                    mode = Mode::Code;
                    prev = quote;
                    word.clear();
                }
            }
            Mode::Template => {
                if c == '\\' {
                    escaped = true;
                } else if c == '`' {
                    mode = Mode::Code;
                    prev = '`';
                    word.clear();
                }
            }
            Mode::LineComment => {
                if c == '\n' {
                    mode = Mode::Code;
                }
            }
            Mode::BlockComment => {
                if c == '*' && next == Some('/') {
                    mode = Mode::Code;
                    i += 1;
                }
            }
            Mode::Regex => {
                if c == '\\' {
                    escaped = true;
                } else if c == '[' {
                    mode = Mode::RegexClass;
                } else if c == '/' {
                    mode = Mode::Code;
                    prev = '/';
                    word.clear();
                } else if c == '\n' {
                    return Err(format!(
                        "line {line}: newline inside regular expression opened on line {lit_line}"
                    ));
                }
            }
            Mode::RegexClass => {
                if c == '\\' {
                    escaped = true;
                } else if c == ']' {
                    mode = Mode::Regex;
                } else if c == '\n' {
                    return Err(format!(
                        "line {line}: newline inside regular expression opened on line {lit_line}"
                    ));
                }
            }
        }
        if c == '\n' {
            line += 1;
        }
        i += 1;
    }
    match mode {
        Mode::Code | Mode::LineComment => {}
        Mode::Single | Mode::Double => {
            return Err(format!(
                "unterminated string literal opened on line {lit_line}"
            ));
        }
        Mode::Template => {
            return Err(format!(
                "unterminated template literal opened on line {lit_line}"
            ));
        }
        Mode::BlockComment => return Err("unterminated block comment".to_owned()),
        Mode::Regex | Mode::RegexClass => {
            return Err(format!(
                "unterminated regular expression opened on line {lit_line}"
            ));
        }
    }
    if let Some((open, at)) = stack.pop() {
        return Err(format!("unclosed `{open}` opened on line {at}"));
    }
    Ok(())
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Division or regex? The standard heuristic: after a value (identifier,
/// number, closer, literal) a slash divides; after an operator, opener, or
/// keyword that expects an expression it starts a regex.
fn regex_can_start(prev: char, word: &str) -> bool {
    if prev == '\0' {
        return true;
    }
    if matches!(
        word,
        "return"
            | "typeof"
            | "case"
            | "in"
            | "of"
            | "new"
            | "delete"
            | "void"
            | "instanceof"
            | "do"
            | "else"
            | "yield"
            | "await"
            | "throw"
    ) {
        return true;
    }
    matches!(
        prev,
        '(' | '['
            | '{'
            | ','
            | ';'
            | ':'
            | '='
            | '!'
            | '&'
            | '|'
            | '?'
            | '+'
            | '-'
            | '*'
            | '%'
            | '<'
            | '>'
            | '~'
            | '^'
    )
}

#[cfg(test)]
mod tests {
    use super::js_syntax_error;
    use ferrite_tabs::protocol::{
        ABOUT_HTML, AUTOFILL_HTML, BLOCKED_HTML, BOOKMARKS_HTML, BOOT_HTML, CERTS_HTML,
        CONTEXT_HTML, CRASHED_HTML, CREDITS_HTML, DIALOG_HTML, DOWNLOADS_HTML, ERROR_HTML,
        FIND_HTML, HISTORY_HTML, NEWTAB_HTML, PASSWORDS_HTML, PDF_HTML, PROFILES_HTML,
        READER_DRIVER_JS, READER_HTML, RESTORE_HTML, SETTINGS_HTML, SITE_HTML, TABSEARCH_HTML,
        TASKMANAGER_HTML, TOAST_HTML, VIEWSOURCE_HTML, WELCOME_HTML,
    };

    const CHROME_HTML: &str = include_str!("../../ferrite-chrome/assets/chrome.html");
    const CHROME_MOBILE_HTML: &str = include_str!("../../ferrite-chrome/assets/chrome_mobile.html");

    /// Inline `<script>` bodies with the HTML line each one starts on.
    fn inline_scripts(html: &str) -> Vec<(usize, &str)> {
        let lower = html.to_ascii_lowercase();
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(rel) = lower[from..].find("<script") {
            let open = from + rel;
            let Some(gt) = html[open..].find('>') else {
                break;
            };
            let body_start = open + gt + 1;
            let Some(end) = lower[body_start..].find("</script>") else {
                break;
            };
            let body = &html[body_start..body_start + end];
            out.push((html[..body_start].matches('\n').count() + 1, body));
            from = body_start + end + "</script>".len();
        }
        out
    }

    fn check_page(name: &str, html: &str) {
        for (index, (html_line, body)) in inline_scripts(html).into_iter().enumerate() {
            if body.trim().is_empty() {
                continue;
            }
            if let Err(e) = js_syntax_error(body) {
                panic!("{name} script {} (html line {html_line}): {e}", index + 1);
            }
        }
    }

    #[test]
    fn every_served_page_and_chrome_asset_parses() {
        // Every privileged page that ships an inline renderer. A page whose
        // script fails to parse is dead in the app even though the source
        // still contains every hook a `contains()` assertion looks for.
        let pages: [(&str, &str); 27] = [
            ("newtab", NEWTAB_HTML),
            ("tabsearch", TABSEARCH_HTML),
            ("ctxmenu", CONTEXT_HTML),
            ("dialog", DIALOG_HTML),
            ("find", FIND_HTML),
            ("toast", TOAST_HTML),
            ("passwords", PASSWORDS_HTML),
            ("autofill", AUTOFILL_HTML),
            ("about", ABOUT_HTML),
            ("credits", CREDITS_HTML),
            ("taskmanager", TASKMANAGER_HTML),
            ("certs", CERTS_HTML),
            ("viewsource", VIEWSOURCE_HTML),
            ("reader", READER_HTML),
            ("welcome", WELCOME_HTML),
            ("profiles", PROFILES_HTML),
            ("restore", RESTORE_HTML),
            ("blocked", BLOCKED_HTML),
            ("error", ERROR_HTML),
            ("crashed", CRASHED_HTML),
            ("settings", SETTINGS_HTML),
            ("history", HISTORY_HTML),
            ("site", SITE_HTML),
            ("pdf", PDF_HTML),
            ("downloads", DOWNLOADS_HTML),
            ("bookmarks", BOOKMARKS_HTML),
            ("boot", BOOT_HTML),
        ];
        for (name, html) in pages {
            check_page(name, html);
        }
        check_page("chrome.html", CHROME_HTML);
        check_page("chrome_mobile.html", CHROME_MOBILE_HTML);
        // The reader driver is a script string, not a page.
        if let Err(e) = js_syntax_error(READER_DRIVER_JS) {
            panic!("reader driver: {e}");
        }
    }

    #[test]
    fn lexer_rejects_the_dead_renderer_shape() {
        // The exact 3c-6 defect: a single-quoted JS string broken across
        // physical lines. Everything else here is a control that proves the
        // lexer is not simply rejecting every slash or quote.
        let broken = "h+='<button aria-checked=\"'+(i)+'\"\n data-color=\"'+i+'\"></button>';";
        assert!(js_syntax_error(broken).is_err(), "newline in a string");
        assert!(
            js_syntax_error("var h='<b>'+i+'</b>';").is_ok(),
            "plain concatenation"
        );
        assert!(
            js_syntax_error("var r=/['\"]/g;").is_ok(),
            "quotes inside a regex literal"
        );
        assert!(
            js_syntax_error("var s=\"a\" / 2;").is_ok(),
            "division after a value"
        );
        assert!(js_syntax_error("if (a) { b(); ").is_err(), "unclosed brace");
        assert!(js_syntax_error("/* open").is_err(), "unclosed comment");
        assert!(js_syntax_error("var t=`a\nb`;").is_ok(), "template newline");
    }
}
