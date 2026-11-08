use crate::remend::remend;
use crate::stream::{FrontierWord, WordFocusState};
use crate::theme::{Rgba, Theme};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub const META_CODESPAN: usize = 1 << 0;
pub const META_STRIKE: usize = 1 << 1;
pub const META_LINK: usize = 1 << 2;
pub const META_MARK: usize = 1 << 3;

#[inline]
pub const fn pack_meta(flags: usize, link_idx: Option<usize>, frontier_idx: Option<usize>) -> usize {
    let l = match link_idx {
        Some(i) if i < 254 => (i + 1) & 0xFF,
        _ => 0,
    };
    let f = match frontier_idx {
        Some(i) if i < 254 => (i + 1) & 0xFF,
        _ => 0,
    };
    (flags & 0xFF) | (l << 8) | (f << 16)
}

#[inline]
pub const fn unpack_link_idx(meta: usize) -> Option<usize> {
    let raw = (meta >> 8) & 0xFF;
    if raw == 0 {
        None
    } else {
        Some(raw - 1)
    }
}

#[inline]
pub const fn unpack_frontier_idx(meta: usize) -> Option<usize> {
    let raw = (meta >> 16) & 0xFF;
    if raw == 0 {
        None
    } else {
        Some(raw - 1)
    }
}

#[derive(Debug, Clone)]
pub struct RichSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub monospace: bool,
    pub color: Rgba,
    pub meta: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListMarker {
    Bullet,
    Ordered(u64),
    Task(bool),
}

#[derive(Debug, Clone)]
pub enum MdBlock {
    Heading {
        level: u8,
        spans: Vec<RichSpan>,
    },
    Paragraph {
        spans: Vec<RichSpan>,
    },
    ListItem {
        depth: usize,
        marker: ListMarker,
        spans: Vec<RichSpan>,
    },
    BlockQuote {
        spans: Vec<RichSpan>,
    },
    CodeBlock {
        lang: String,
        raw_code: String,
        spans: Vec<RichSpan>,
        #[allow(dead_code)]
        live: bool,
    },
    Table {
        headers: Vec<Vec<RichSpan>>,
        rows: Vec<Vec<Vec<RichSpan>>>,
    },
    Rule,
}

#[derive(Debug, Clone)]
pub struct ParsedMarkdown {
    pub blocks: Vec<MdBlock>,
    pub links: Vec<String>,
    pub frontier_states: Vec<WordFocusState>,
}

#[derive(Clone, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    mark: bool,
    link_idx: Option<usize>,
}

pub fn parse_markdown(
    shown_src: &str,
    streaming: bool,
    frontier: &[FrontierWord],
    now_secs: f64,
    theme: &Theme,
) -> ParsedMarkdown {
    let healed = if streaming {
        remend(shown_src)
    } else {
        shown_src.to_string()
    };

    // Map active frontier words to evaluated focus states
    let mut active_words: Vec<(usize, usize, usize)> = Vec::new();
    let mut frontier_states: Vec<WordFocusState> = Vec::new();
    for fw in frontier {
        if let Some(st) = fw.evaluate(now_secs) {
            if frontier_states.len() < 250 {
                let id = frontier_states.len();
                frontier_states.push(st);
                active_words.push((fw.start, fw.end, id));
            }
        }
    }

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(&healed, opts).into_offset_iter();

    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut style = InlineStyle::default();

    let mut current_spans: Vec<RichSpan> = Vec::new();
    let mut in_heading: Option<u8> = None;
    let mut blockquote_depth: usize = 0;
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut item_marker: Option<ListMarker> = None;

    let mut in_code_block: Option<String> = None;
    let mut code_buf = String::new();
    let mut code_frontier = Vec::new();

    let mut in_table = false;
    let mut in_table_head = false;
    let mut table_headers: Vec<Vec<RichSpan>> = Vec::new();
    let mut table_rows: Vec<Vec<Vec<RichSpan>>> = Vec::new();
    let mut current_row: Vec<Vec<RichSpan>> = Vec::new();

    let flush_item_or_para = |blocks: &mut Vec<MdBlock>,
                              spans: &mut Vec<RichSpan>,
                              list_stack: &[Option<u64>],
                              item_marker: &mut Option<ListMarker>,
                              blockquote_depth: usize| {
        if spans.is_empty() {
            return;
        }
        let taken = std::mem::take(spans);
        if let Some(marker) = item_marker.take() {
            blocks.push(MdBlock::ListItem {
                depth: list_stack.len().saturating_sub(1),
                marker,
                spans: taken,
            });
        } else if !list_stack.is_empty() {
            blocks.push(MdBlock::ListItem {
                depth: list_stack.len().saturating_sub(1),
                marker: ListMarker::Bullet,
                spans: taken,
            });
        } else if blockquote_depth > 0 {
            blocks.push(MdBlock::BlockQuote { spans: taken });
        } else {
            blocks.push(MdBlock::Paragraph { spans: taken });
        }
    };

    for (event, range) in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    in_heading = Some(match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    });
                    current_spans.clear();
                }
                Tag::Paragraph => {
                    if !in_table {
                        // Inside list items with multiple paragraphs, flush previous if non-empty
                        if !current_spans.is_empty() {
                            flush_item_or_para(
                                &mut blocks,
                                &mut current_spans,
                                &list_stack,
                                &mut item_marker,
                                blockquote_depth,
                            );
                        }
                    }
                }
                Tag::BlockQuote(_) => {
                    blockquote_depth += 1;
                }
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(l) => {
                            l.split_whitespace().next().unwrap_or("").to_string()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    in_code_block = Some(lang);
                    code_buf.clear();
                    code_frontier.clear();
                }
                Tag::List(start_num) => {
                    if !current_spans.is_empty() {
                        flush_item_or_para(
                            &mut blocks,
                            &mut current_spans,
                            &list_stack,
                            &mut item_marker,
                            blockquote_depth,
                        );
                    }
                    list_stack.push(start_num);
                }
                Tag::Item => {
                    let marker = match list_stack.last_mut() {
                        Some(Some(n)) => {
                            let cur = *n;
                            *n += 1;
                            ListMarker::Ordered(cur)
                        }
                        _ => ListMarker::Bullet,
                    };
                    item_marker = Some(marker);
                    current_spans.clear();
                }
                Tag::Table(_) => {
                    in_table = true;
                    table_headers.clear();
                    table_rows.clear();
                }
                Tag::TableHead => {
                    in_table_head = true;
                    current_row.clear();
                }
                Tag::TableRow => {
                    current_row.clear();
                }
                Tag::TableCell => {
                    current_spans.clear();
                }
                Tag::Emphasis => style.italic = true,
                Tag::Strong => style.bold = true,
                Tag::Strikethrough => style.strike = true,
                Tag::Link { dest_url, .. } => {
                    let id = links.len();
                    links.push(dest_url.to_string());
                    style.link_idx = Some(id);
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    let level = in_heading.take().unwrap_or(2);
                    if !current_spans.is_empty() {
                        blocks.push(MdBlock::Heading {
                            level,
                            spans: std::mem::take(&mut current_spans),
                        });
                    }
                }
                TagEnd::Paragraph => {
                    if !in_table {
                        flush_item_or_para(
                            &mut blocks,
                            &mut current_spans,
                            &list_stack,
                            &mut item_marker,
                            blockquote_depth,
                        );
                    }
                }
                TagEnd::BlockQuote(_) => {
                    if !current_spans.is_empty() {
                        blocks.push(MdBlock::BlockQuote {
                            spans: std::mem::take(&mut current_spans),
                        });
                    }
                    blockquote_depth = blockquote_depth.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    let lang = in_code_block.take().unwrap_or_default();
                    let raw = code_buf.trim_end_matches('\n').to_string();
                    let highlighted = map_code_frontier(highlight_code(&raw, &lang, theme), &code_frontier);
                    let is_last_open = streaming && range.end >= healed.len().saturating_sub(4);
                    blocks.push(MdBlock::CodeBlock {
                        lang,
                        raw_code: raw,
                        spans: highlighted,
                        live: is_last_open,
                    });
                    code_buf.clear();
                }
                TagEnd::Item => {
                    flush_item_or_para(
                        &mut blocks,
                        &mut current_spans,
                        &list_stack,
                        &mut item_marker,
                        blockquote_depth,
                    );
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                }
                TagEnd::TableCell => {
                    current_row.push(std::mem::take(&mut current_spans));
                }
                TagEnd::TableHead => {
                    table_headers = std::mem::take(&mut current_row);
                    in_table_head = false;
                }
                TagEnd::TableRow => {
                    if !in_table_head && !current_row.is_empty() {
                        table_rows.push(std::mem::take(&mut current_row));
                    }
                }
                TagEnd::Table => {
                    in_table = false;
                    blocks.push(MdBlock::Table {
                        headers: std::mem::take(&mut table_headers),
                        rows: std::mem::take(&mut table_rows),
                    });
                }
                TagEnd::Emphasis => style.italic = false,
                TagEnd::Strong => style.bold = false,
                TagEnd::Strikethrough => style.strike = false,
                TagEnd::Link => style.link_idx = None,
                _ => {}
            },
            Event::TaskListMarker(checked) => {
                item_marker = Some(ListMarker::Task(checked));
            }
            Event::Text(text) => {
                if in_code_block.is_some() {
                    for &(start, end, index) in &active_words {
                        let a = start.max(range.start);
                        let b = end.min(range.end).min(range.start + text.len());
                        if b > a {
                            code_frontier.push((code_buf.len() + a - range.start,
                                code_buf.len() + b - range.start, index));
                        }
                    }
                    code_buf.push_str(&text);
                } else {
                    let base_color = if blockquote_depth > 0 {
                        theme.muted
                    } else {
                        theme.fg
                    };
                    append_styled_text_with_frontier(
                        &mut current_spans,
                        &text,
                        range.start,
                        &style,
                        in_heading.is_some(),
                        base_color,
                        &active_words,
                    );
                }
            }
            Event::Code(code) => {
                let f_idx = find_overlapping_frontier(range.start, range.end, &active_words);
                let mut flags = META_CODESPAN;
                if style.strike {
                    flags |= META_STRIKE;
                }
                if style.link_idx.is_some() {
                    flags |= META_LINK;
                }
                // Pad inline code slightly with thin spaces for clean pill breathing room
                current_spans.push(RichSpan {
                    text: format!(" {} ", code),
                    bold: false,
                    italic: false,
                    monospace: true,
                    color: theme.fg,
                    meta: pack_meta(flags, style.link_idx, f_idx),
                });
            }
            Event::SoftBreak => {
                if in_code_block.is_some() {
                    code_buf.push('\n');
                } else {
                    current_spans.push(RichSpan {
                        text: " ".to_string(),
                        bold: style.bold || in_heading.is_some(),
                        italic: style.italic,
                        monospace: false,
                        color: theme.fg,
                        meta: 0,
                    });
                }
            }
            Event::HardBreak => {
                if in_code_block.is_some() {
                    code_buf.push('\n');
                } else {
                    current_spans.push(RichSpan {
                        text: "\n".to_string(),
                        bold: false,
                        italic: false,
                        monospace: false,
                        color: theme.fg,
                        meta: 0,
                    });
                }
            }
            Event::Rule => {
                blocks.push(MdBlock::Rule);
            }
            _ => {}
        }
    }

    if let Some(lang) = in_code_block.take() {
        let raw = code_buf.trim_end_matches('\n').to_string();
        let spans = map_code_frontier(highlight_code(&raw, &lang, theme), &code_frontier);
        blocks.push(MdBlock::CodeBlock {
            lang,
            raw_code: raw,
            spans,
            live: streaming,
        });
    } else if !current_spans.is_empty() {
        flush_item_or_para(
            &mut blocks,
            &mut current_spans,
            &list_stack,
            &mut item_marker,
            blockquote_depth,
        );
    }

    ParsedMarkdown {
        blocks,
        links,
        frontier_states,
    }
}

fn find_overlapping_frontier(
    start: usize,
    end: usize,
    active_words: &[(usize, usize, usize)],
) -> Option<usize> {
    for &(ws, we, id) in active_words.iter().rev() {
        if start < we && end > ws {
            return Some(id);
        }
    }
    None
}

fn append_styled_text_with_frontier(
    out: &mut Vec<RichSpan>,
    text: &str,
    base_offset: usize,
    style: &InlineStyle,
    force_bold: bool,
    color: Rgba,
    active_words: &[(usize, usize, usize)],
) {
    // Parse inline `==highlight==` segments first
    let mut cursor = 0usize;
    while cursor < text.len() {
        if let Some(rel_open) = text[cursor..].find("==") {
            let open_at = cursor + rel_open;
            if open_at > cursor {
                emit_words_segment(
                    out,
                    &text[cursor..open_at],
                    base_offset + cursor,
                    style,
                    false,
                    force_bold,
                    color,
                    active_words,
                );
            }
            let after_open = open_at + 2;
            if let Some(rel_close) = text[after_open..].find("==") {
                let close_at = after_open + rel_close;
                emit_words_segment(
                    out,
                    &text[after_open..close_at],
                    base_offset + after_open,
                    style,
                    true,
                    force_bold,
                    color,
                    active_words,
                );
                cursor = close_at + 2;
                continue;
            } else {
                emit_words_segment(
                    out,
                    &text[open_at..],
                    base_offset + open_at,
                    style,
                    false,
                    force_bold,
                    color,
                    active_words,
                );
                break;
            }
        } else {
            emit_words_segment(
                out,
                &text[cursor..],
                base_offset + cursor,
                style,
                false,
                force_bold,
                color,
                active_words,
            );
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_words_segment(
    out: &mut Vec<RichSpan>,
    seg: &str,
    seg_offset: usize,
    style: &InlineStyle,
    is_mark: bool,
    force_bold: bool,
    color: Rgba,
    active_words: &[(usize, usize, usize)],
) {
    if seg.is_empty() {
        return;
    }
    let mut flags = 0usize;
    if style.strike {
        flags |= META_STRIKE;
    }
    if style.link_idx.is_some() {
        flags |= META_LINK;
    }
    if is_mark || style.mark {
        flags |= META_MARK;
    }

    // Fast path: no active blur frontier overlapping this text event
    if active_words.is_empty()
        || seg_offset + seg.len() <= active_words.first().map(|w| w.0).unwrap_or(usize::MAX)
    {
        out.push(RichSpan {
            text: seg.to_string(),
            bold: style.bold || force_bold,
            italic: style.italic,
            monospace: false,
            color,
            meta: pack_meta(flags, style.link_idx, None),
        });
        return;
    }

    // Split into word + trailing-space tokens so each frontier word gets its own optical focus state
    let bytes = seg.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let start = i;
        if bytes[i].is_ascii_whitespace() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(RichSpan {
                text: seg[start..i].to_string(),
                bold: style.bold || force_bold,
                italic: style.italic,
                monospace: false,
                color,
                meta: pack_meta(flags, style.link_idx, None),
            });
        } else {
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let word_end = i;
            // Attach trailing horizontal spaces to the word span so kerning & spacing stay seamless
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            let f_idx = find_overlapping_frontier(
                seg_offset + start,
                seg_offset + word_end,
                active_words,
            );
            out.push(RichSpan {
                text: seg[start..i].to_string(),
                bold: style.bold || force_bold,
                italic: style.italic,
                monospace: false,
                color,
                meta: pack_meta(flags, style.link_idx, f_idx),
            });
        }
    }
}

/// Preserve syntax colours while projecting source-byte frontier ranges into
/// code-local offsets. Code and prose then share the same optical compositor.
fn map_code_frontier(spans: Vec<RichSpan>, words: &[(usize, usize, usize)]) -> Vec<RichSpan> {
    let mut out = Vec::new();
    let mut offset = 0;
    for span in spans {
        let end = offset + span.text.len();
        let mut boundaries = vec![0, span.text.len()];
        for &(a, b, _) in words {
            for point in [a, b] {
                if point > offset && point < end && span.text.is_char_boundary(point - offset) {
                    boundaries.push(point - offset);
                }
            }
        }
        boundaries.sort_unstable(); boundaries.dedup();
        for pair in boundaries.windows(2) {
            let mut part = span.clone();
            part.text = span.text[pair[0]..pair[1]].into();
            let index = find_overlapping_frontier(offset + pair[0], offset + pair[1], words);
            part.meta = pack_meta(0, None, index);
            out.push(part);
        }
        offset = end;
    }
    out
}

/// Lightweight, zero-dependency Quiet Paper syntax tokenizer for fenced code blocks.
pub fn highlight_code(code: &str, lang: &str, theme: &Theme) -> Vec<RichSpan> {
    if code.is_empty() {
        return Vec::new();
    }
    let l = lang.to_ascii_lowercase();
    let hash_comments = matches!(
        l.as_str(),
        "py" | "python" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "yml" | "ruby" | "nix"
    );
    let slash_comments = !matches!(l.as_str(), "py" | "python" | "toml" | "yaml" | "yml" | "sh" | "bash");

    let mut spans: Vec<RichSpan> = Vec::new();
    let bytes = code.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;

    let push_span = |spans: &mut Vec<RichSpan>, text: &str, color: Rgba, italic: bool| {
        if text.is_empty() {
            return;
        }
        if let Some(last) = spans.last_mut() {
            if last.color == color && last.italic == italic {
                last.text.push_str(text);
                return;
            }
        }
        spans.push(RichSpan {
            text: text.to_string(),
            bold: false,
            italic,
            monospace: true,
            color,
            meta: 0,
        });
    };

    while i < len {
        // Line comment `//` or `#`
        if (slash_comments && i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/')
            || (hash_comments && bytes[i] == b'#')
        {
            let start = i;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            push_span(&mut spans, &code[start..i], theme.syn_comment, true);
            continue;
        }

        // Block comment `/* ... */`
        if slash_comments && i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(len);
            push_span(&mut spans, &code[start..i], theme.syn_comment, true);
            continue;
        }

        // String literal `"..."` or `'...'` (avoiding Rust lifetimes `'a`)
        if bytes[i] == b'"'
            || (bytes[i] == b'\''
                && !(i + 2 < len
                    && bytes[i + 1].is_ascii_alphabetic()
                    && bytes[i + 2] != b'\''))
        {
            let quote = bytes[i];
            let start = i;
            i += 1;
            while i < len && bytes[i] != b'\n' {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(len);
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            push_span(&mut spans, &code[start..i], theme.syn_string, false);
            continue;
        }

        // Numeric literal
        if bytes[i].is_ascii_digit()
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
        {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_hexdigit()
                    || matches!(bytes[i], b'x' | b'o' | b'b' | b'.' | b'_'))
            {
                i += 1;
            }
            push_span(&mut spans, &code[start..i], theme.syn_number, false);
            continue;
        }

        // Identifier or keyword
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let ident = &code[start..i];
            let color = if is_keyword(ident) {
                theme.syn_keyword
            } else if is_type_ident(ident) {
                theme.syn_type
            } else {
                theme.fg
            };
            push_span(&mut spans, ident, color, false);
            continue;
        }

        // UTF-8 character fallback (operators, whitespace, punctuation, Unicode)
        let ch = code[i..].chars().next().unwrap();
        let next_i = i + ch.len_utf8();
        push_span(&mut spans, &code[i..next_i], theme.fg, false);
        i = next_i;
    }

    spans
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "fn" | "let" | "mut" | "pub" | "struct" | "enum" | "impl" | "trait" | "use" | "mod"
            | "match" | "if" | "else" | "for" | "while" | "loop" | "return" | "break"
            | "continue" | "async" | "await" | "move" | "where" | "const" | "static" | "type"
            | "self" | "Self" | "super" | "crate" | "as" | "in" | "ref" | "unsafe" | "dyn"
            | "def" | "class" | "import" | "from" | "lambda" | "yield" | "with" | "try"
            | "except" | "finally" | "raise" | "pass" | "None" | "True" | "False" | "and"
            | "or" | "not" | "function" | "var" | "export" | "default" | "interface"
            | "extends" | "new" | "typeof" | "null" | "undefined" | "true" | "false"
            | "switch" | "case" | "go" | "defer" | "chan" | "package" | "select"
            | "local" | "then" | "fi" | "do" | "done" | "esac"
    )
}

fn is_type_ident(w: &str) -> bool {
    matches!(
        w,
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64"
            | "i128" | "isize" | "f32" | "f64" | "bool" | "char" | "str" | "String" | "Vec"
            | "Option" | "Result" | "Some" | "Ok" | "Err" | "Box" | "Arc" | "Rc"
    ) || (w.len() >= 2
        && w.as_bytes()[0].is_ascii_uppercase()
        && w.bytes().any(|b| b.is_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeMode;

    #[test]
    fn parses_headings_lists_code_and_highlights() {
        let theme = Theme::for_mode(ThemeMode::Dark);
        let md = "## Title\n\nHello ==world== and `inline`.\n\n- [x] Done\n\n```rust\nfn main() {}\n```";
        let doc = parse_markdown(md, false, &[], 0.0, &theme);
        assert_eq!(doc.blocks.len(), 4);
        assert!(matches!(doc.blocks[0], MdBlock::Heading { level: 2, .. }));
        assert!(matches!(doc.blocks[1], MdBlock::Paragraph { .. }));
        assert!(matches!(
            doc.blocks[2],
            MdBlock::ListItem {
                marker: ListMarker::Task(true),
                ..
            }
        ));
        assert!(matches!(doc.blocks[3], MdBlock::CodeBlock { .. }));
    }
}

#[cfg(test)]
mod code_stream_regressions {
    use super::*;
    #[test]
    fn fenced_syntax_keeps_frontier_metadata_and_unicode_text() {
        let text = "```rust\nlet café = hello();\n```";
        let start = text.find("hello").unwrap();
        let word = FrontierWord { start, end: start + 5, birth_secs: 1.0 };
        let doc = parse_markdown(text, true, &[word], 1.05, &Theme::for_mode(crate::theme::ThemeMode::Dark));
        match &doc.blocks[0] {
            MdBlock::CodeBlock { spans, raw_code, .. } => {
                assert_eq!(spans.iter().map(|s| s.text.as_str()).collect::<String>(), *raw_code);
                assert!(spans.iter().any(|s| s.text.contains("hello") && unpack_frontier_idx(s.meta) == Some(0)));
            }
            _ => panic!("expected fenced code"),
        }
    }
}

#[cfg(test)]
mod partial_markdown_stress {
    use super::*;
    #[test]
    fn unicode_and_incomplete_components_keep_valid_spans_at_every_boundary() {
        let source = "# café **bold**\n\n| A | B |\n|---|---|\n| `x` | العربية |\n\n```rust\nlet café = \"हिन्दी\";\n```\n\n[link](https://github.com) ==mark== ~~strike~~";
        let theme = Theme::for_mode(crate::theme::ThemeMode::Dark);
        for end in source.char_indices().map(|(i, _)| i).chain(std::iter::once(source.len())) {
            let doc = parse_markdown(&source[..end], true, &[], 0.0, &theme);
            for block in doc.blocks {
                if let MdBlock::CodeBlock { raw_code, spans, .. } = block {
                    assert_eq!(raw_code, spans.iter().map(|s| s.text.as_str()).collect::<String>());
                }
            }
        }
    }
}
