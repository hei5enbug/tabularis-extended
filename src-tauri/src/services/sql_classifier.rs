//! SQL statement classification shared by the MCP `run_query` tool and the
//! common service layer. It depends on nothing outside `std` so the service
//! contracts crate can compile it on its own.

/// Classify the kind of SQL: `"select"`, `"write"`, `"ddl"`, or `"unknown"`.
///
/// Conservative on purpose: only a statement proven to be a single read
/// returns `"select"`. `SELECT ... INTO`, `EXEC`, `EXECUTE`, `CALL`, multiple
/// statements and anything else the scanner cannot identify never do, so
/// callers can fail closed when enforcing safety policies (read-only mode,
/// approval gates).
///
/// Dialects disagree about where a string literal or quoted identifier ends
/// (MySQL backslash escapes, PostgreSQL dollar quoting, SQL Server brackets),
/// and that decides where a `;` is a visible statement separator. The text is
/// scanned under every interpretation and the result is kept only when all of
/// them agree; any disagreement is `"unknown"`.
pub fn classify_query_kind(sql: &str) -> &'static str {
    const VARIANTS: [StripOptions; 4] = [
        StripOptions::STANDARD,
        StripOptions {
            backslash_escapes: true,
            ..StripOptions::STANDARD
        },
        StripOptions {
            brackets: true,
            ..StripOptions::STANDARD
        },
        StripOptions {
            dollar_quotes: true,
            ..StripOptions::STANDARD
        },
    ];
    let mut agreed: Option<&'static str> = None;
    for options in VARIANTS {
        let kind = classify_stripped(&strip_with(sql, options));
        match agreed {
            None => agreed = Some(kind),
            Some(previous) if previous != kind => return "unknown",
            Some(_) => {}
        }
    }
    agreed.unwrap_or("unknown")
}

fn classify_stripped(stripped: &str) -> &'static str {
    let trimmed = stripped.trim_start();
    if trimmed.is_empty() {
        return "unknown";
    }
    let trimmed_upper = trimmed.to_uppercase();

    if is_create_compound_routine(&trimmed_upper) {
        return "ddl";
    }

    // Fail closed for multi-statement payloads: a leading `SELECT 1; DROP …`
    // must NOT be tagged as a clean read just because the first keyword is
    // SELECT — the read-only and approval gates rely on this classification.
    if has_trailing_statements(stripped) {
        return "unknown";
    }
    // Peel any leading `(` (and surrounding whitespace) before reading the
    // first keyword. A parenthesized query expression such as
    // `(SELECT ...) UNION ALL (SELECT ...)` — the shape you get when each
    // UNION branch needs its own ORDER BY / LIMIT — starts with `(`, so the
    // first keyword would otherwise come back empty and fall through to
    // "unknown", needlessly tripping the read-only and approval gates for a
    // pure read. Multi-statement payloads are already rejected above, so the
    // inner leading keyword is a safe basis for classification.
    let peeled_upper = trimmed
        .trim_start_matches(|c: char| c == '(' || c.is_whitespace())
        .to_uppercase();
    let first = first_keyword(&peeled_upper);

    match first.as_str() {
        // EXPLAIN needs option-aware handling: plain EXPLAIN only plans, but
        // EXPLAIN ANALYZE (and EXPLAIN (ANALYZE ...)) *executes* the wrapped
        // statement. Classifying it blindly as a read would let
        // `EXPLAIN ANALYZE DELETE …` run past read-only mode and the approval
        // gate. See `classify_explain`.
        "EXPLAIN" => classify_explain(&peeled_upper),
        // `SELECT ... INTO` creates a table or writes a file or variable.
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "PRAGMA" | "VALUES"
            if contains_keyword(&peeled_upper, "INTO") =>
        {
            "write"
        }
        // `PRAGMA name = value` changes database settings.
        "PRAGMA" if peeled_upper.contains('=') => "unknown",
        "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "PRAGMA" | "VALUES" => {
            read_unless_embedded_statement(&peeled_upper, &first)
        }
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE" => "write",
        "CREATE" | "DROP" | "ALTER" | "TRUNCATE" | "RENAME" | "GRANT" | "REVOKE" | "COMMENT" => {
            "ddl"
        }
        "WITH" => match classify_cte(&peeled_upper) {
            "select" if contains_keyword(&peeled_upper, "INTO") => "write",
            "select" => read_unless_embedded_statement(&peeled_upper, &first),
            kind => kind,
        },
        // EXEC, EXECUTE and CALL run code the scanner cannot see into.
        _ => "unknown",
    }
}

fn is_create_compound_routine(upper_sql: &str) -> bool {
    if first_keyword(upper_sql) != "CREATE" {
        return false;
    }

    let header = upper_sql.split("BEGIN").next().unwrap_or(upper_sql);
    let is_routine = ["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT"]
        .iter()
        .any(|keyword| contains_keyword(header, keyword));
    if !is_routine {
        return false;
    }

    let without_terminal_semicolon = upper_sql.trim_end().trim_end_matches(';').trim_end();
    if !without_terminal_semicolon.ends_with("END") {
        return false;
    }

    !has_complete_end_before_terminal_statement(without_terminal_semicolon)
}

fn has_complete_end_before_terminal_statement(upper_sql: &str) -> bool {
    let terminal_end = match upper_sql.rfind("END") {
        Some(index) => index,
        None => return false,
    };
    let before_terminal = &upper_sql[..terminal_end];
    let bytes = before_terminal.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    for i in 0..bytes.len().saturating_sub(2) {
        if &bytes[i..i + 3] != b"END" {
            continue;
        }
        let prev_ok = i == 0 || !is_word(bytes[i - 1]);
        let next_ok = i + 3 == bytes.len() || !is_word(bytes[i + 3]);
        if !prev_ok || !next_ok {
            continue;
        }

        let mut cursor = i + 3;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b';' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b';')
        {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
            return true;
        }
    }

    false
}

/// Returns true when `stripped` contains a semicolon followed by additional
/// non-whitespace SQL content — i.e., more than one statement.
///
/// The input must already have been run through `strip_impl`, which replaces
/// string-literal, comment, and quoted-identifier bytes with whitespace, so
/// any `;` that survives here is a real statement terminator under the
/// escaping interpretation used to produce `stripped`.
pub(crate) fn has_trailing_statements(stripped: &str) -> bool {
    let mut found_semi = false;
    for c in stripped.chars() {
        if found_semi && !c.is_whitespace() {
            return true;
        }
        if c == ';' {
            found_semi = true;
        }
    }
    false
}

/// Words that begin a write, DDL or control statement. T-SQL and other
/// dialects accept a second statement after a read without any separator
/// (`SELECT 1 DELETE FROM t`), so a read is only proven when none of these
/// appear after the leading keyword.
const STATEMENT_STARTERS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "MERGE", "UPSERT", "REPLACE", "TRUNCATE", "CREATE", "ALTER",
    "DROP", "RENAME", "GRANT", "REVOKE", "DENY", "EXEC", "EXECUTE", "CALL", "DECLARE", "SET",
    "BEGIN", "COMMIT", "ROLLBACK", "SAVE", "USE", "BACKUP", "RESTORE", "DBCC", "KILL", "SHUTDOWN",
    "RECONFIGURE", "BULK", "OPENROWSET", "OPENDATASOURCE", "OPENQUERY", "WAITFOR", "GO", "COPY",
    "VACUUM", "LOCK", "LOAD", "ATTACH", "DETACH", "PRAGMA", "HANDLER", "IMPORT", "INTO",
];

/// String functions that share a name with a statement keyword.
const FUNCTION_LIKE_STARTERS: &[&str] = &["INSERT", "REPLACE", "TRUNCATE"];

fn read_unless_embedded_statement(upper: &str, first: &str) -> &'static str {
    if has_embedded_statement(upper, first) {
        "unknown"
    } else {
        "select"
    }
}

/// True when a whole-word statement starter appears at any nesting depth after
/// the leading keyword. `upper` is the stripped, upper-cased SQL.
fn has_embedded_statement(upper: &str, first: &str) -> bool {
    let bytes = upper.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let skip_leading_create = matches!(first, "SHOW" | "DESCRIBE" | "DESC");
    let mut cursor = first.len().min(bytes.len());
    let mut first_token = true;
    while cursor < bytes.len() {
        if !is_word(bytes[cursor]) {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && is_word(bytes[cursor]) {
            cursor += 1;
        }
        let token = &upper[start..cursor];
        let was_first = std::mem::replace(&mut first_token, false);
        if !STATEMENT_STARTERS.contains(&token) {
            continue;
        }
        let rest = upper[cursor..].trim_start();
        let bracketed = start > 0 && bytes[start - 1] == b'[' && bytes.get(cursor) == Some(&b']');
        let function_call = FUNCTION_LIKE_STARTERS.contains(&token) && rest.starts_with('(');
        let show_create = token == "CREATE" && skip_leading_create && was_first;
        let index_hint = token == "USE" && (rest.starts_with("INDEX") || rest.starts_with("KEY"));
        if bracketed || function_call || show_create || index_hint {
            continue;
        }
        return true;
    }
    false
}

fn first_keyword(upper: &str) -> String {
    upper
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect()
}

fn classify_cte(stripped_upper: &str) -> &'static str {
    let ddl = ["CREATE", "DROP", "ALTER", "TRUNCATE", "RENAME"];
    let dml = ["INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE"];
    for kw in &ddl {
        if contains_keyword(stripped_upper, kw) {
            return "ddl";
        }
    }
    for kw in &dml {
        if contains_keyword(stripped_upper, kw) {
            return "write";
        }
    }
    "select"
}

fn contains_keyword(haystack: &str, needle: &str) -> bool {
    keyword_index(haystack, needle).is_some()
}

/// Byte offset of the first whole-word occurrence of `needle` in `haystack`,
/// or `None`. "Whole word" means the match is not flanked by an identifier
/// byte on either side, so `ANALYZE` does not match inside `analyze_runs`.
fn keyword_index(haystack: &str, needle: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let nbytes = needle.as_bytes();
    let nlen = nbytes.len();
    if nlen == 0 || bytes.len() < nlen {
        return None;
    }
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for i in 0..=bytes.len() - nlen {
        if &bytes[i..i + nlen] == nbytes {
            let prev_ok = i == 0 || !is_word(bytes[i - 1]);
            let next_ok = i + nlen == bytes.len() || !is_word(bytes[i + nlen]);
            if prev_ok && next_ok {
                return Some(i);
            }
        }
    }
    None
}

/// Classify an `EXPLAIN …` statement, whose kind depends on its options.
///
/// A plain `EXPLAIN` only produces a plan and never touches data, so it is a
/// read. But `EXPLAIN ANALYZE …` (legacy syntax) and `EXPLAIN (ANALYZE …) …`
/// (parenthesized options) *run* the wrapped statement to gather actual timing
/// — so an `ANALYZE` option makes the whole thing as dangerous as the wrapped
/// statement itself. We therefore detect an `ANALYZE` option and, when present,
/// classify the wrapped statement (`write`/`ddl`/`unknown`) instead of `select`.
///
/// `explain_upper` must be the stripped, upper-cased SQL whose first keyword is
/// `EXPLAIN`.
fn classify_explain(explain_upper: &str) -> &'static str {
    let after = explain_upper["EXPLAIN".len()..].trim_start();
    let (options, wrapped) = split_explain_options(after);

    // Only ANALYZE causes execution; every other option (VERBOSE, BUFFERS,
    // FORMAT …, SQLite's QUERY PLAN, MySQL's FORMAT=JSON) is plan-only.
    if !contains_keyword(options, "ANALYZE") {
        return "select";
    }
    classify_wrapped_statement(wrapped)
}

/// Split the text following `EXPLAIN` into `(options, wrapped_statement)`.
///
/// Handles both the parenthesized form `(ANALYZE, BUFFERS) SELECT …` and the
/// bare form `ANALYZE VERBOSE SELECT …`. For the bare form the option list runs
/// up to the wrapped statement's first keyword. Fails closed (everything is
/// "options", wrapped is empty) when a leading `(` is never closed.
fn split_explain_options(after: &str) -> (&str, &str) {
    if after.starts_with('(') {
        let bytes = after.as_bytes();
        let mut depth = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return (&after[1..i], after[i + 1..].trim_start());
                    }
                }
                _ => {}
            }
        }
        return (after, "");
    }
    match first_wrapped_statement_index(after) {
        Some(idx) => (&after[..idx], &after[idx..]),
        None => (after, ""),
    }
}

/// Byte offset of the earliest statement keyword that could begin the target of
/// an `EXPLAIN`, used to find where a bare option list ends.
fn first_wrapped_statement_index(upper: &str) -> Option<usize> {
    const KEYWORDS: &[&str] = &[
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "VALUES", "TABLE", "WITH",
        "CREATE", "DROP", "ALTER", "TRUNCATE", "RENAME",
    ];
    KEYWORDS
        .iter()
        .filter_map(|kw| keyword_index(upper, kw))
        .min()
}

/// Classify the statement wrapped by an executing `EXPLAIN ANALYZE`, as if it
/// had been submitted directly. Unrecognized/empty input is `"unknown"` so the
/// safety gates fail closed.
fn classify_wrapped_statement(wrapped_upper: &str) -> &'static str {
    let peeled = wrapped_upper.trim_start_matches(|c: char| c == '(' || c.is_whitespace());
    match first_keyword(peeled).as_str() {
        "SELECT" | "VALUES" | "TABLE" if contains_keyword(peeled, "INTO") => "write",
        first @ ("SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "PRAGMA" | "VALUES" | "TABLE") => {
            read_unless_embedded_statement(peeled, first)
        }
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE" => "write",
        "CREATE" | "DROP" | "ALTER" | "TRUNCATE" | "RENAME" | "GRANT" | "REVOKE" | "COMMENT" => {
            "ddl"
        }
        "WITH" => match classify_cte(peeled) {
            "select" => read_unless_embedded_statement(peeled, "WITH"),
            kind => kind,
        },
        _ => "unknown",
    }
}

/// Replace string literals and SQL comments with whitespace so keyword
/// scanning cannot be fooled by tokens that live inside a value, comment,
/// or quoted identifier.
///
/// Uses the SQL-standard reading of single-quoted strings (only `''` escapes
/// a quote). For the backslash-aware reading used to harden multi-statement
/// detection against MySQL-style escaping, see [`strip_impl`].
pub fn strip_strings_and_comments(sql: &str) -> String {
    strip_impl(sql, false)
}

/// Which quoting rules the scanner applies besides comments, `'…'` strings,
/// `"…"` and backtick identifiers.
#[derive(Clone, Copy)]
struct StripOptions {
    /// A `\` inside a single-quoted string escapes the next byte (MySQL).
    backslash_escapes: bool,
    /// `[…]` quotes an identifier, `]]` escapes `]` (SQL Server).
    brackets: bool,
    /// `$tag$ … $tag$` is a string literal (PostgreSQL).
    dollar_quotes: bool,
}
impl StripOptions {
    const STANDARD: Self = Self {
        backslash_escapes: false,
        brackets: false,
        dollar_quotes: false,
    };
}

fn strip_impl(sql: &str, backslash_escapes: bool) -> String {
    strip_with(
        sql,
        StripOptions {
            backslash_escapes,
            ..StripOptions::STANDARD
        },
    )
}

fn dollar_tag_len(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'$') {
        return None;
    }
    if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        return None;
    }
    let mut end = start + 1;
    if bytes
        .get(end)
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        while bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            end += 1;
        }
    }
    (bytes.get(end) == Some(&b'$')).then_some(end + 1 - start)
}

fn strip_with(sql: &str, options: StripOptions) -> String {
    let backslash_escapes = options.backslash_escapes;
    let bytes = sql.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if options.dollar_quotes {
            if let Some(tag) = dollar_tag_len(bytes, i) {
                let delimiter = &bytes[i..i + tag];
                let mut j = i + tag;
                let mut closed = false;
                while j + tag <= bytes.len() {
                    if &bytes[j..j + tag] == delimiter {
                        j += tag;
                        closed = true;
                        break;
                    }
                    j += 1;
                }
                let end = if closed { j } else { bytes.len() };
                out.extend(std::iter::repeat(b' ').take(end - i));
                i = end;
                continue;
            }
        }
        if options.brackets && c == b'[' {
            out.push(b' ');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b']' && bytes.get(i + 1) == Some(&b']') {
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if bytes[i] == b']' {
                    out.push(b' ');
                    i += 1;
                    break;
                } else {
                    out.push(b' ');
                    i += 1;
                }
            }
            continue;
        }
        // Line comment
        if c == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        // Block comment (non-nesting; sufficient for classification)
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                out.push(b' ');
                i += 1;
            }
            if i + 1 < bytes.len() {
                out.push(b' ');
                out.push(b' ');
                i += 2;
            } else {
                while i < bytes.len() {
                    out.push(b' ');
                    i += 1;
                }
            }
            continue;
        }
        // Single-quoted string literal (SQL-style escaped quote: '')
        if c == b'\'' {
            out.push(b' ');
            i += 1;
            while i < bytes.len() {
                if backslash_escapes && bytes[i] == b'\\' && i + 1 < bytes.len() {
                    // MySQL-style backslash escape: the next byte is part of
                    // the string regardless of what it is (including `'`).
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else if bytes[i] == b'\'' && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else if bytes[i] == b'\'' {
                    out.push(b' ');
                    i += 1;
                    break;
                } else {
                    out.push(b' ');
                    i += 1;
                }
            }
            continue;
        }
        // Double-quoted identifier (PostgreSQL / ANSI)
        if c == b'"' {
            out.push(b' ');
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                out.push(b' ');
                i += 1;
            }
            if i < bytes.len() {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        // Backtick-quoted identifier (MySQL)
        if c == b'`' {
            out.push(b' ');
            i += 1;
            while i < bytes.len() && bytes[i] != b'`' {
                out.push(b' ');
                i += 1;
            }
            if i < bytes.len() {
                out.push(b' ');
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}
