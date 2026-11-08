/// `remend`: heals partial (streaming) Markdown strings so unterminated inline/block syntax
/// renders cleanly on every frame without flickering raw markers.
/// Faithfully ported from `final-chat/src/lib/streammark/remend.ts` in pure zero-regex Rust.

pub fn remend(src: &str) -> String {
    if src.is_empty() {
        return String::new();
    }

    // 1. Count fenced code block delimiters (` ``` ` or ` ~~~ ` with <= 3 leading spaces).
    let mut fence_count = 0usize;
    let mut fence_marker = "```";
    for line in src.lines() {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        if indent <= 3 {
            if trimmed.starts_with("```") {
                fence_count += 1;
                fence_marker = "```";
            } else if trimmed.starts_with("~~~") {
                fence_count += 1;
                fence_marker = "~~~";
            }
        }
    }

    if fence_count % 2 == 1 {
        let mut clean = src.to_string();
        if clean.ends_with("\n``") {
            clean.truncate(clean.len() - 2);
        } else if clean.ends_with("\n`") {
            clean.truncate(clean.len() - 1);
        }
        if !clean.ends_with('\n') {
            clean.push('\n');
        }
        clean.push_str(fence_marker);
        return clean;
    }

    // 2. Strip dangling partial fence or incomplete HTML tag at the very tail.
    let mut text = src.to_string();
    if text.ends_with("\n``") {
        text.truncate(text.len() - 2);
    } else if text.ends_with("\n`") {
        text.truncate(text.len() - 1);
    }
    if let Some(lt_idx) = text.rfind('<') {
        let after = &text[lt_idx + 1..];
        if !after.contains('>') && after.len() <= 16 {
            let valid_tag_prefix = after
                .strip_prefix('/')
                .unwrap_or(after)
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == '-');
            if valid_tag_prefix && !after.is_empty() {
                text.truncate(lt_idx);
            }
        }
    }

    // 3. Operate on the active paragraph tail (after the last `\n\n`).
    let split_at = text.rfind("\n\n").map(|i| i + 2).unwrap_or(0);
    let head = &text[..split_at];
    let mut tail = text[split_at..].to_string();

    // 4. Inline single-backtick code span: inside it nothing else parses as Markdown.
    let single_ticks = count_isolated_backticks(&tail);
    if single_ticks % 2 == 1 {
        let mut out = String::with_capacity(head.len() + tail.len() + 1);
        out.push_str(head);
        out.push_str(&tail);
        out.push('`');
        return out;
    }

    // Mask out completed inline code spans when counting emphasis markers so `*` inside `a * b` is ignored.
    let masked = mask_inline_code(&tail);

    // 5. Highlight (`==`): hide an unclosed opener rather than closing prematurely,
    //    so highlights sweep in smoothly once completed.
    let eq_positions = find_double_token(&masked, b'=');
    if eq_positions.len() % 2 == 1 {
        let last_pos = *eq_positions.last().unwrap();
        tail.replace_range(last_pos..last_pos + 2, "");
    }

    // 6. Unclosed `[label](url` at the end -> heal to `[label](#)`.
    if let Some(healed) = heal_dangling_link(&tail) {
        tail = healed;
    }
    let masked = mask_inline_code(&tail);

    // 7. Strip trailing bare emphasis openers (`**`, `*`, `~~`, `__`) that have no body yet.
    for bare in ["**", "~~", "__", "*"] {
        if masked.ends_with(bare) && !is_list_bullet(&masked) {
            // Only strip if this bare suffix actually makes the count odd
            let probe = &masked[..masked.len() - bare.len()];
            let odd_with = is_unclosed_marker(&masked, bare);
            let odd_without = is_unclosed_marker(probe, bare);
            if odd_with && !odd_without {
                let new_len = tail.len() - bare.len();
                tail.truncate(new_len);
                break;
            }
        }
    }

    let masked = mask_inline_code(&tail);
    let mut closers: Vec<&'static str> = Vec::new();
    if is_unclosed_marker(&masked, "~~") {
        closers.push("~~");
    }
    if is_unclosed_marker(&masked, "*") {
        closers.push("*");
    }
    if is_unclosed_marker(&masked, "**") {
        closers.push("**");
    }
    if is_unclosed_marker(&masked, "__") {
        closers.push("__");
    }

    let mut out = String::with_capacity(head.len() + tail.len() + 8);
    out.push_str(head);
    out.push_str(&tail);
    for c in closers.into_iter().rev() {
        out.push_str(c);
    }
    out
}

fn count_isolated_backticks(s: &str) -> usize {
    let b = s.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let start = i;
            while i < b.len() && b[i] == b'`' {
                i += 1;
            }
            if i - start == 1 {
                count += 1;
            }
        } else {
            i += 1;
        }
    }
    count
}

fn mask_inline_code(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut in_code = false;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            in_code = !in_code;
            out.push(b'`');
            i += 1;
        } else if in_code {
            out.push(b' ');
            i += 1;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn find_double_token(s: &str, ch: u8) -> Vec<usize> {
    let b = s.as_bytes();
    let mut pos = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == ch && b[i + 1] == ch && (i + 2 >= b.len() || b[i + 2] != ch) {
            pos.push(i);
            i += 2;
        } else {
            i += 1;
        }
    }
    pos
}

fn is_list_bullet(s: &str) -> bool {
    let last_line = s.rsplit('\n').next().unwrap_or(s).trim_start_matches(' ');
    last_line == "*" || last_line.starts_with("* ")
}

fn is_unclosed_marker(masked: &str, marker: &str) -> bool {
    match marker {
        "**" => find_double_token(masked, b'*').len() % 2 == 1,
        "~~" => find_double_token(masked, b'~').len() % 2 == 1,
        "__" => find_double_token(masked, b'_').len() % 2 == 1,
        "*" => {
            // Strip `**` and leading list bullets `* ` before counting single `*`
            let mut count = 0usize;
            for line in masked.lines() {
                let trimmed = line.trim_start_matches(' ');
                let body = trimmed.strip_prefix("* ").unwrap_or(line);
                let bytes = body.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'*' {
                        let start = i;
                        while i < bytes.len() && bytes[i] == b'*' {
                            i += 1;
                        }
                        if (i - start) % 2 == 1 {
                            count += 1;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            count % 2 == 1
        }
        _ => false,
    }
}

fn heal_dangling_link(tail: &str) -> Option<String> {
    let close_bracket = tail.rfind("](")?;
    let after_paren = &tail[close_bracket + 2..];
    if after_paren.contains(')') || after_paren.contains('\n') {
        return None;
    }
    let before = &tail[..close_bracket];
    let open_bracket = before.rfind('[')?;
    let label = &tail[open_bracket + 1..close_bracket];
    let mut healed = String::with_capacity(tail.len() + 4);
    healed.push_str(&tail[..open_bracket]);
    healed.push('[');
    healed.push_str(label);
    healed.push_str("](#)");
    Some(healed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_unterminated_code_fence() {
        let s = "Here is Rust:\n```rust\nfn main() {";
        assert_eq!(remend(s), "Here is Rust:\n```rust\nfn main() {\n```");
    }

    #[test]
    fn closes_bold_and_italic_cleanly() {
        assert_eq!(remend("Hello **world"), "Hello **world**");
        assert_eq!(remend("Hello *italic"), "Hello *italic*");
        assert_eq!(remend("Hello **"), "Hello ");
    }

    #[test]
    fn ignores_asterisks_inside_inline_code() {
        assert_eq!(remend("Use `a * b` in Rust"), "Use `a * b` in Rust");
        assert_eq!(remend("Use `a * b"), "Use `a * b`");
    }

    #[test]
    fn hides_unclosed_highlight_until_complete() {
        assert_eq!(remend("This is ==important"), "This is important");
        assert_eq!(remend("This is ==important=="), "This is ==important==");
    }

    #[test]
    fn heals_partial_markdown_link() {
        assert_eq!(
            remend("Read [the docs](https://example.com/pa"),
            "Read [the docs](#)"
        );
    }
}
