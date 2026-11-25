//! Text for a terminal: safe, measured in columns, wrapped, and dates.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn width(text: &str) -> usize {
    text.width()
}

/// Characters that would move the cursor, reorder text, or hide in it.
/// Record text is written by anyone, so it never reaches the terminal as is.
fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

/// One safe line: line breaks and tabs become spaces.
pub fn line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !out.is_empty();
        } else if !unsafe_char(c) {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    out
}

/// One line of an editor: safe, with its spaces.
pub fn raw_line(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !unsafe_char(*c))
        .collect()
}

/// Safe text that keeps its lines and indentation.
pub fn block(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            c if unsafe_char(c) => {}
            c => out.push(c),
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// The text of HTML, for journal entries and articles.
pub fn html(text: &str) -> String {
    let text = text.replace("[code]", "").replace("[/code]", "");
    if !text.contains('<') && !text.contains('&') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        let tag = &rest[at + 1..];
        let opens = tag.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!');
        let Some(end) = tag.find('>').filter(|_| opens) else {
            out.push('<');
            rest = tag;
            continue;
        };
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &tag[end + 1..];
        let fresh = out.is_empty() || out.ends_with('\n');
        match name.as_str() {
            "br" => out.push('\n'),
            "p" | "div" | "tr" | "h1" | "h2" | "h3" | "h4" | "ul" | "ol" | "table" | "pre"
                if !fresh =>
            {
                out.push('\n');
            }
            "li" if !closing => out.push_str(if fresh { "- " } else { "\n- " }),
            "script" | "style" if !closing => {
                let close = format!("</{name}");
                // ASCII lowercase keeps byte positions.
                rest = match rest.to_ascii_lowercase().find(&close) {
                    Some(at) => &rest[at..],
                    None => "",
                };
            }
            _ => {}
        }
    }
    out.push_str(rest);
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_owned()
}

/// The text cut to `max` columns, with an ellipsis when something is lost.
pub fn cut(text: &str, max: usize, ellipsis: &str) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let mark = if width(ellipsis) <= max { ellipsis } else { "" };
    let room = max - width(mark);
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push_str(mark);
    out
}

/// The text in exactly `to` columns.
pub fn fit(text: &str, to: usize, ellipsis: &str) -> String {
    let mut out = cut(text, to, ellipsis);
    let used = width(&out);
    out.extend(std::iter::repeat_n(' ', to.saturating_sub(used)));
    out
}

/// Byte ranges of the visual lines of one text line at `max` columns.
/// Breaks after spaces where it can. Every byte is in exactly one range.
pub fn wrap_ranges(text: &str, max: usize) -> Vec<std::ops::Range<usize>> {
    let max = max.max(1);
    let mut out = Vec::new();
    let (mut start, mut used) = (0, 0);
    // The last place after a space, and the columns used up to it.
    let mut space: Option<(usize, usize)> = None;
    for (at, c) in text.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > max && at > start && c != ' ' {
            match space.filter(|(byte, _)| *byte > start) {
                Some((byte, before)) => {
                    out.push(start..byte);
                    start = byte;
                    used -= before;
                }
                None => {
                    out.push(start..at);
                    start = at;
                    used = 0;
                }
            }
            space = None;
        }
        used += w;
        if c == ' ' {
            space = Some((at + 1, used));
        }
    }
    out.push(start..text.len());
    out
}

pub fn wrap(text: &str, max: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|l| {
            wrap_ranges(l, max)
                .into_iter()
                .map(|r| l[r].trim_end().to_owned())
        })
        .collect()
}

/// 1234567 as `1,234,567`.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A count that fits in four columns: 999, 1.2k, 34k, 1.2M.
pub fn short_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{}.{}k", n / 1_000, n % 1_000 / 100),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        1_000_000..10_000_000 => format!("{}.{}M", n / 1_000_000, n % 1_000_000 / 100_000),
        _ => format!("{}M", n / 1_000_000),
    }
}

pub fn bytes(n: u64) -> String {
    match n {
        0..1_024 => format!("{n} B"),
        1_024..1_048_576 => format!("{:.1} KB", n as f64 / 1_024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", grouped(n as u64), if n == 1 { one } else { many })
}

/// `sys_updated_on` as `Updated on`, `u_cost_center` as `Cost center`.
pub fn humanize(field: &str) -> String {
    let last = field.rsplit('.').next().unwrap_or(field);
    let name = last
        .trim_start_matches("sys_")
        .trim_start_matches("u_")
        .replace('_', " ");
    let mut chars = name.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => field.to_owned(),
    }
}

/// Seconds since 1970 of `YYYY-MM-DD` or `YYYY-MM-DD HH:MM[:SS]` in UTC, which
/// is how the instance stores dates.
pub fn parse_utc(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = match text.split_once([' ', 'T']) {
        Some((date, time)) => (date, time.trim_end_matches('Z')),
        None => (text, ""),
    };
    let mut parts = date.split('-');
    let year: i64 = parts.next().filter(|y| y.len() == 4)?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }
    let mut clock = [0i64; 3];
    if !time.is_empty() {
        let mut parts = time.split(':');
        for (i, limit) in [(0, 23), (1, 59), (2, 59)] {
            match parts.next() {
                Some(part) => clock[i] = part.parse().ok().filter(|n| (0..=limit).contains(n))?,
                None if i == 2 => {}
                None => return None,
            }
        }
        if parts.next().is_some() {
            return None;
        }
    }
    Some(days(year, month, day) * 86_400 + clock[0] * 3_600 + clock[1] * 60 + clock[2])
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a date of the Gregorian calendar.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// `YYYY-MM-DD HH:MM:SS` in UTC.
pub fn format_utc(seconds: i64) -> String {
    let (day, clock) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        clock / 3_600,
        clock / 60 % 60,
        clock % 60
    )
}

/// How far away a time is: `now`, `5m`, `3h`, `2d`, `6w`, `4mo`, `2y`, and
/// `in 2d` for the future.
pub fn ago(then: i64, now: i64) -> String {
    let gap = (now - then).abs();
    let text = match gap {
        0..60 => return "now".into(),
        60..3_600 => format!("{}m", gap / 60),
        3_600..86_400 => format!("{}h", gap / 3_600),
        86_400..1_209_600 => format!("{}d", gap / 86_400),
        1_209_600..5_184_000 => format!("{}w", gap / 604_800),
        5_184_000..63_072_000 => format!("{}mo", gap / 2_592_000),
        _ => format!("{}y", gap / 31_536_000),
    };
    if then > now {
        format!("in {text}")
    } else {
        text
    }
}

/// The same as a phrase: `now`, `5m ago`, `in 2d`.
pub fn since(then: i64, now: i64) -> String {
    match ago(then, now) {
        gap if gap == "now" || then > now => gap,
        gap => format!("{gap} ago"),
    }
}

/// How well `needle` fuzzy-matches `text`: `None` when its letters are not
/// all there in order. Whole-word and prefix matches score higher.
pub fn fuzzy(needle: &str, text: &str) -> Option<i32> {
    let text = text.to_lowercase();
    let mut total = 0;
    for word in needle.to_lowercase().split_whitespace() {
        total += fuzzy_word(word, &text)?;
    }
    Some(total - text.len() as i32 / 8)
}

fn fuzzy_word(word: &str, text: &str) -> Option<i32> {
    if let Some(at) = text.find(word) {
        let boundary = text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        return Some(if at == 0 {
            120
        } else if boundary {
            100
        } else {
            60
        });
    }
    let (mut score, mut run, mut before) = (0, 0, ' ');
    let mut wanted = word.chars().peekable();
    for c in text.chars() {
        let Some(&next) = wanted.peek() else { break };
        if c == next {
            wanted.next();
            run += 1;
            score += run + if before.is_alphanumeric() { 0 } else { 8 };
        } else {
            run = 0;
            score -= 1;
        }
        before = c;
    }
    (wanted.peek().is_none() && score > -10).then_some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_text_cannot_control_the_terminal() {
        assert_eq!(line("a\u{1b}[31mb\r\n\tc\u{202e}d"), "a[31mb cd");
        assert_eq!(block("a\r\n\tb\u{7}\n\n"), "a\n    b");
    }

    #[test]
    fn text_fits_columns_with_wide_characters() {
        assert_eq!(fit("東京オフィス", 7, "…"), "東京オ…");
        assert_eq!(fit("東京オフィス", 8, "…"), "東京オ… ");
        assert_eq!(cut("abc", 2, "..."), "ab");
        assert_eq!(fit("ab", 4, "…"), "ab  ");
        assert_eq!(wrap("one two three", 8), ["one two", "three"]);
        assert_eq!(wrap("東京東京", 5), ["東京", "東京"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
        let text = "supercalifragilistic word";
        let ranges = wrap_ranges(text, 6);
        assert_eq!(
            ranges.iter().map(|r| &text[r.clone()]).collect::<String>(),
            text
        );
    }

    #[test]
    fn dates_round_trip_and_read_as_distance() {
        for (text, seconds) in [
            ("1970-01-01 00:00:00", 0),
            ("2000-02-29 23:59:59", 951_868_799),
            ("2026-11-25 08:30:00", 1_795_595_400),
            ("1969-12-31 00:00:00", -86_400),
        ] {
            assert_eq!(parse_utc(text), Some(seconds), "{text}");
            assert_eq!(format_utc(seconds), text);
        }
        assert_eq!(parse_utc("2026-02-30"), None);
        assert_eq!(parse_utc("2026-01-01 25:00"), None);
        assert_eq!(parse_utc("25/11/2026"), None);
        assert_eq!(ago(0, 30), "now");
        assert_eq!(ago(0, 7_200), "2h");
        assert_eq!(ago(200_000, 0), "in 2d");
        assert_eq!(since(0, 30), "now");
        assert_eq!(since(0, 7_200), "2h ago");
        assert_eq!(since(200_000, 0), "in 2d");
    }

    #[test]
    fn html_becomes_text() {
        assert_eq!(
            html("[code]<p>Hi <b>Ada</b></p><ul><li>one</li></ul>a &lt; b[/code]"),
            "Hi Ada\n- one\na < b"
        );
        assert_eq!(html("if (a < b) return"), "if (a < b) return");
        assert_eq!(html("x<script>alert(1)</script>y"), "xy");
    }

    #[test]
    fn fuzzy_prefers_prefixes_and_words() {
        assert!(fuzzy("inc", "Incidents") > fuzzy("inc", "Principal"));
        assert!(fuzzy("upd set", "Update sets").is_some());
        assert!(fuzzy("xyz", "Update sets").is_none());
    }
}
