//! SQL statement classification shared by the MCP `run_query` tool and the
//! common service layer. It depends on nothing outside `std` so the service
//! contracts crate can compile it on its own.

/// The four ways a statement's quoting can be read.
const QUOTING_VARIANTS: [StripOptions; 4] = [
    StripOptions::STANDARD,
    StripOptions {
        backslash_escapes: true,
        ..StripOptions::STANDARD
    },
    StripOptions {
        brackets: true,
        nested_comments: true,
        ..StripOptions::STANDARD
    },
    StripOptions {
        dollar_quotes: true,
        nested_comments: true,
        ..StripOptions::STANDARD
    },
];

/// True when `sql` holds exactly one statement under every quoting
/// interpretation. A trailing `;` is allowed; text with nothing but comments
/// or whitespace is not a statement.
pub fn is_single_statement(sql: &str) -> bool {
    QUOTING_VARIANTS.into_iter().all(|options| {
        let stripped = strip_with(sql, options);
        !stripped.trim().trim_matches(';').trim().is_empty() && !has_trailing_statements(&stripped)
    })
}

/// Statements an estimated plan can be asked for.
const PLANNABLE_KEYWORDS: &[&str] = &[
    "SELECT", "WITH", "INSERT", "UPDATE", "DELETE", "MERGE", "VALUES", "TABLE",
];

/// True when `sql` is one statement, under every quoting interpretation, that
/// starts with a keyword an estimated plan can be asked for. `EXPLAIN`, `SET`,
/// `EXEC`, DDL, `USE` and transaction control are not among them.
pub fn is_plannable_statement(sql: &str) -> bool {
    is_single_statement(sql)
        && leading_keyword(sql)
            .is_some_and(|keyword| PLANNABLE_KEYWORDS.contains(&keyword.as_str()))
}

/// The upper-cased first keyword of `sql`, after leading `(` and whitespace,
/// when every quoting interpretation reads the same one.
pub fn leading_keyword(sql: &str) -> Option<String> {
    let mut agreed: Option<String> = None;
    for options in QUOTING_VARIANTS {
        let stripped = strip_with(sql, options);
        let peeled = stripped.trim_start_matches(|c: char| c == '(' || c.is_whitespace());
        let keyword = first_keyword(&peeled.to_uppercase());
        if keyword.is_empty() || agreed.as_ref().is_some_and(|previous| *previous != keyword) {
            return None;
        }
        agreed = Some(keyword);
    }
    agreed
}

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
    const VARIANTS: [StripOptions; 4] = QUOTING_VARIANTS;
    let mut agreed: Option<&'static str> = None;
    for options in VARIANTS {
        let kind = classify_stripped(&strip_with(sql, options));
        match agreed {
            None => agreed = Some(kind),
            Some(previous) if previous != kind => return "unknown",
            Some(_) => {}
        }
    }
    match agreed {
        // MySQL runs the text of `/*! … */` and `/*M! … */` comments, which
        // every variant blanks, so a read cannot be proven around them.
        Some(_) if has_executable_comment(sql) => "unknown",
        Some("select") if has_lone_carriage_return(sql) => "unknown",
        Some("select") if leading_keyword_token(sql, false).is_none() => "unknown",
        Some("select") if calls_side_effect_function(sql, &VARIANTS) => "write",
        Some(kind) => kind,
        None => "unknown",
    }
}

fn has_executable_comment(sql: &str) -> bool {
    let upper = sql.to_ascii_uppercase();
    upper.contains("/*!") || upper.contains("/*M!")
}

/// SQL Server and PostgreSQL end a `--` comment at a carriage return, which the
/// scanner reads as part of the comment, so text after a lone `\r` is unseen.
fn has_lone_carriage_return(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    (0..bytes.len()).any(|index| bytes[index] == b'\r' && bytes.get(index + 1) != Some(&b'\n'))
}

/// The first word of `sql` after whitespace, comments, `(` and, with
/// `semicolons`, `;`, when it is a plain keyword. T-SQL runs a leading name as
/// a procedure, so a quoted name (`"sp_updatestats"`) or a word that goes on
/// with `$`, `#`, `@`, a digit, `_` or a non-ASCII letter (`PRINT$wipe`) is not
/// one.
fn leading_keyword_token(sql: &str, semicolons: bool) -> Option<&str> {
    let mut rest = sql;
    loop {
        rest = skip_trivia(rest)?;
        match rest.as_bytes().first() {
            Some(b'(') => rest = &rest[1..],
            Some(b';') if semicolons => rest = &rest[1..],
            _ => break,
        }
    }
    let length = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    let continues = rest.as_bytes().get(length).is_some_and(|byte| {
        !byte.is_ascii()
            || byte.is_ascii_alphanumeric()
            || matches!(byte, b'_' | b'$' | b'#' | b'@')
    });
    (length > 0 && !continues).then(|| &rest[..length])
}

/// Drivers whose read batches can be proven beyond a single `SELECT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadDialect {
    Postgres,
    SqlServer,
}

/// A read the host proved safe to run without write intent. `text` is what the
/// driver must run and `settings` are session settings, in statement order, it
/// must apply first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvenRead {
    pub text: String,
    pub settings: Vec<(String, String)>,
}

/// The read dialect of a saved connection's driver id.
pub fn read_dialect(driver: &str) -> Option<ReadDialect> {
    match driver {
        "postgresql-plus" => Some(ReadDialect::Postgres),
        "sqlserver-plus" => Some(ReadDialect::SqlServer),
        _ => None,
    }
}

/// Prove `sql` is a read. A single read that [`classify_query_kind`] calls
/// `"select"` is proven for every dialect. With a dialect, one read wrapped in
/// a transaction and listed `SET` statements (PostgreSQL), or a T-SQL batch
/// that only reads (SQL Server), is proven too. Any doubt is `None`.
pub fn prove_read(sql: &str, dialect: Option<ReadDialect>) -> Option<ProvenRead> {
    match classify_query_kind(sql) {
        "select" => {
            return Some(ProvenRead {
                text: sql.to_owned(),
                settings: Vec::new(),
            })
        }
        "write" | "ddl" => return None,
        _ if has_lone_carriage_return(sql) => return None,
        _ => {}
    }
    match dialect? {
        ReadDialect::Postgres => prove_postgres_wrapper(sql),
        ReadDialect::SqlServer => prove_sql_server_batch(sql).then(|| ProvenRead {
            text: sql.to_owned(),
            settings: Vec::new(),
        }),
    }
}

/// Session settings a proven PostgreSQL read may carry.
const PG_FORWARDED_SETTINGS: &[&str] = &[
    "statement_timeout",
    "lock_timeout",
    "idle_in_transaction_session_timeout",
    "plan_cache_mode",
    "work_mem",
    "search_path",
    "jit",
    "random_page_cost",
    "max_parallel_workers_per_gather",
    "enable_seqscan",
    "enable_indexscan",
    "enable_indexonlyscan",
    "enable_bitmapscan",
    "enable_nestloop",
    "enable_hashjoin",
    "enable_mergejoin",
];

/// Settings a read already runs under, so asking for them is accepted and
/// dropped when the value keeps the transaction read-only.
const PG_READ_ONLY_SETTINGS: &[&str] = &["transaction_read_only", "default_transaction_read_only"];

enum PgToken {
    Word(String),
    Text(String),
    Quoted(String),
    Number(String),
    Comma,
    Equals,
}

impl PgToken {
    fn is_word(&self, expected: &str) -> bool {
        matches!(self, Self::Word(word) if word.eq_ignore_ascii_case(expected))
    }
}

enum PgPiece<'a> {
    Begin,
    End,
    Drop,
    Setting(String, String),
    Read(&'a str),
}

/// Byte offsets of every `;` that is a statement separator, when every quoting
/// interpretation finds the same ones.
fn agreed_semicolons(sql: &str) -> Option<Vec<usize>> {
    let mut agreed: Option<Vec<usize>> = None;
    for options in QUOTING_VARIANTS {
        let stripped = strip_with(sql, options);
        if stripped.len() != sql.len() {
            return None;
        }
        let positions: Vec<usize> = stripped
            .bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b';')
            .map(|(index, _)| index)
            .collect();
        match &agreed {
            None => agreed = Some(positions),
            Some(previous) if *previous != positions => return None,
            Some(_) => {}
        }
    }
    agreed
}

/// `text` without leading whitespace and comments. Nested or unterminated
/// block comments are read differently by different dialects, so they fail.
fn skip_trivia(mut text: &str) -> Option<&str> {
    loop {
        text = text.trim_start();
        if text.starts_with("--") {
            text = text.find('\n').map_or("", |index| &text[index + 1..]);
        } else if let Some(body) = text.strip_prefix("/*") {
            let end = body.find("*/")?;
            if body[..end].contains("/*") || body.starts_with('/') || body[..end].ends_with('/') {
                return None;
            }
            text = &body[end + 2..];
        } else {
            return Some(text);
        }
    }
}

fn pg_tokens(piece: &str) -> Option<Vec<PgToken>> {
    let mut tokens = Vec::new();
    let mut rest = skip_trivia(piece)?;
    while let Some(&first) = rest.as_bytes().first() {
        let (token, length) = match first {
            b',' => (PgToken::Comma, 1),
            b'=' => (PgToken::Equals, 1),
            b'\'' => {
                let mut value = String::new();
                let mut index = 1;
                loop {
                    let end = rest[index..].find('\'')?;
                    value.push_str(&rest[index..index + end]);
                    index += end + 1;
                    if rest[index..].starts_with('\'') {
                        value.push('\'');
                        index += 1;
                    } else {
                        break;
                    }
                }
                if value.contains('\0') {
                    return None;
                }
                (PgToken::Text(value), index)
            }
            b'"' => {
                let end = rest[1..].find('"')?;
                let body = &rest[1..1 + end];
                if body.is_empty() || body.contains('\0') || rest[end + 2..].starts_with('"') {
                    return None;
                }
                (PgToken::Quoted(rest[..end + 2].to_owned()), end + 2)
            }
            b'0'..=b'9' => {
                let bytes = rest.as_bytes();
                let mut index = 0;
                while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                    index += 1;
                }
                if bytes.get(index) == Some(&b'.')
                    && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
                {
                    index += 1;
                    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                        index += 1;
                    }
                }
                while bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
                    index += 1;
                }
                if bytes
                    .get(index)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    return None;
                }
                (PgToken::Number(rest[..index].to_owned()), index)
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let length = rest
                    .bytes()
                    .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    .count();
                (PgToken::Word(rest[..length].to_owned()), length)
            }
            _ => return None,
        };
        tokens.push(token);
        rest = skip_trivia(&rest[length..])?;
    }
    Some(tokens)
}

fn leading_word(text: &str) -> &str {
    let length = text
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        .count();
    &text[..length]
}

fn pg_piece(piece: &str) -> Option<Option<PgPiece<'_>>> {
    let rest = skip_trivia(piece)?;
    if rest.is_empty() {
        return Some(None);
    }
    let first = leading_word(rest).to_ascii_uppercase();
    Some(Some(match first.as_str() {
        "BEGIN" | "START" => {
            let tokens = pg_tokens(rest)?;
            let mut tail = &tokens[1..];
            let mode = tail
                .first()
                .is_some_and(|token| token.is_word("TRANSACTION"));
            let work = tail.first().is_some_and(|token| token.is_word("WORK"));
            if mode || (work && first == "BEGIN") {
                tail = &tail[1..];
            } else if first == "START" {
                return None;
            }
            if let [read, only] = tail {
                if read.is_word("READ") && only.is_word("ONLY") {
                    tail = &[];
                }
            }
            if !tail.is_empty() {
                return None;
            }
            PgPiece::Begin
        }
        "COMMIT" | "ROLLBACK" | "END" | "ABORT" => {
            let tokens = pg_tokens(rest)?;
            match &tokens[1..] {
                [] => {}
                [word] if word.is_word("WORK") || word.is_word("TRANSACTION") => {}
                _ => return None,
            }
            PgPiece::End
        }
        "SET" => pg_set(&pg_tokens(rest)?[1..])?,
        _ => PgPiece::Read(piece),
    }))
}

fn pg_set(tokens: &[PgToken]) -> Option<PgPiece<'static>> {
    if let [transaction, read, only] = tokens {
        if transaction.is_word("TRANSACTION") && read.is_word("READ") && only.is_word("ONLY") {
            return Some(PgPiece::Drop);
        }
    }
    let tokens = match tokens.first() {
        Some(scope) if scope.is_word("LOCAL") || scope.is_word("SESSION") => &tokens[1..],
        _ => tokens,
    };
    let (PgToken::Word(name), separator, value) = (tokens.first()?, tokens.get(1)?, &tokens[2..])
    else {
        return None;
    };
    if !(matches!(separator, PgToken::Equals) || separator.is_word("TO")) {
        return None;
    }
    let name = name.to_ascii_lowercase();
    let read_only = PG_READ_ONLY_SETTINGS.contains(&name.as_str());
    if !read_only && !PG_FORWARDED_SETTINGS.contains(&name.as_str()) {
        return None;
    }
    let mut items = Vec::new();
    for (index, token) in value.iter().enumerate() {
        if index % 2 == 1 {
            if !matches!(token, PgToken::Comma) {
                return None;
            }
            continue;
        }
        items.push(match token {
            PgToken::Text(text) => (text.clone(), false),
            PgToken::Number(number) => (number.clone(), false),
            PgToken::Word(word) if !word.eq_ignore_ascii_case("DEFAULT") => (word.clone(), false),
            PgToken::Quoted(quoted) => (quoted.clone(), true),
            _ => return None,
        });
    }
    if value.len() % 2 == 0 || items.is_empty() {
        return None;
    }
    if read_only {
        let [(value, false)] = items.as_slice() else {
            return None;
        };
        return ["on", "true", "yes", "1"]
            .contains(&value.to_ascii_lowercase().as_str())
            .then_some(PgPiece::Drop);
    }
    if name != "search_path" && (items.len() != 1 || items[0].1) {
        return None;
    }
    let joined: Vec<String> = items.into_iter().map(|(item, _)| item).collect();
    Some(PgPiece::Setting(name, joined.join(", ")))
}

/// One read statement inside `BEGIN`, listed `SET` statements and a closing
/// `COMMIT` or `ROLLBACK`, with the settings the driver has to apply.
fn prove_postgres_wrapper(sql: &str) -> Option<ProvenRead> {
    if has_executable_comment(sql) {
        return None;
    }
    let mut pieces = Vec::new();
    let mut start = 0;
    for end in agreed_semicolons(sql)?.into_iter().chain([sql.len()]) {
        if let Some(piece) = pg_piece(&sql[start..end])? {
            pieces.push(piece);
        }
        start = end + 1;
    }
    let last = pieces.len().checked_sub(1)?;
    let mut read = None;
    let mut settings = Vec::new();
    let mut wrapped = false;
    for (index, piece) in pieces.into_iter().enumerate() {
        match piece {
            PgPiece::Begin if index == 0 => wrapped = true,
            PgPiece::End if index == last => wrapped = true,
            PgPiece::Begin | PgPiece::End => return None,
            // The driver applies settings before the read, so one written
            // after it would change what the read sees.
            PgPiece::Drop | PgPiece::Setting(..) if read.is_some() => return None,
            PgPiece::Drop => wrapped = true,
            PgPiece::Setting(name, value) => {
                wrapped = true;
                settings.push((name, value));
            }
            PgPiece::Read(text) => {
                if read.replace(text).is_some() {
                    return None;
                }
            }
        }
    }
    let text = read?.trim();
    (wrapped && classify_query_kind(text) == "select").then(|| ProvenRead {
        text: text.to_owned(),
        settings,
    })
}

/// Words that open a T-SQL batch the scanner can follow. In T-SQL only the
/// first statement of a batch may call a procedure without `EXEC`, so any other
/// first word fails.
const TSQL_BATCH_STARTERS: &[&str] = &[
    "BEGIN",
    "SELECT",
    "WITH",
    "DECLARE",
    "SET",
    "IF",
    "WHILE",
    "PRINT",
    "THROW",
    "RAISERROR",
    "WAITFOR",
];

/// T-SQL statement words beyond [`STATEMENT_STARTERS`] that a read batch must
/// not use.
const TSQL_EXTRA_STARTERS: &[&str] = &[
    "CHECKPOINT",
    "WRITETEXT",
    "UPDATETEXT",
    "SETUSER",
    "REVERT",
    "RECEIVE",
    "SEND",
    "OPEN",
    "FETCH",
    "CLOSE",
    "DEALLOCATE",
    "CURSOR",
    "ENABLE",
    "DISABLE",
    "MOVE",
    "CONVERSATION",
];

/// Session options a read batch may set.
const TSQL_SET_OPTIONS: &[&str] = &[
    "NOCOUNT",
    "XACT_ABORT",
    "ANSI_NULLS",
    "ANSI_WARNINGS",
    "ANSI_PADDING",
    "ANSI_NULL_DFLT_ON",
    "ANSI_NULL_DFLT_OFF",
    "ARITHABORT",
    "ARITHIGNORE",
    "CONCAT_NULL_YIELDS_NULL",
    "NUMERIC_ROUNDABORT",
    "QUOTED_IDENTIFIER",
    "DATEFIRST",
    "DATEFORMAT",
    "LANGUAGE",
    "LOCK_TIMEOUT",
    "DEADLOCK_PRIORITY",
    "ROWCOUNT",
    "TEXTSIZE",
    "QUERY_GOVERNOR_COST_LIMIT",
];

/// True when `sql` is a T-SQL batch that only reads, under every quoting
/// interpretation.
fn prove_sql_server_batch(sql: &str) -> bool {
    leading_keyword_token(sql, true)
        .is_some_and(|word| TSQL_BATCH_STARTERS.contains(&word.to_ascii_uppercase().as_str()))
        && !has_executable_comment(sql)
        && !calls_side_effect_function(sql, &QUOTING_VARIANTS)
        && QUOTING_VARIANTS
            .into_iter()
            .all(|options| tsql_batch_reads(&strip_with(sql, options).to_ascii_uppercase()))
}

fn tsql_batch_reads(upper: &str) -> bool {
    let peeled = upper.trim_start_matches(|c: char| c == '(' || c == ';' || c.is_whitespace());
    if !TSQL_BATCH_STARTERS.contains(&leading_word(peeled)) {
        return false;
    }
    let bytes = upper.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut cursor = 0;
    let mut previous_word = "";
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
        if hidden_starter(token, TSQL_EXTRA_STARTERS) {
            return false;
        }
        let previous = std::mem::replace(&mut previous_word, token);
        // `@name` is a variable, never a keyword.
        if start > 0 && bytes[start - 1] == b'@' {
            continue;
        }
        let rest = upper[cursor..].trim_start();
        let bracketed = start > 0 && bytes[start - 1] == b'[' && bytes.get(cursor) == Some(&b']');
        if token == "LOG" {
            if !rest.starts_with('(') && !bracketed {
                return false;
            }
            continue;
        }
        if !STATEMENT_STARTERS.contains(&token) && !TSQL_EXTRA_STARTERS.contains(&token) {
            continue;
        }
        // `OFFSET n ROWS FETCH NEXT m ROWS ONLY` pages a read.
        let paging = token == "FETCH" && matches!(previous, "ROW" | "ROWS");
        let function_call = FUNCTION_LIKE_STARTERS.contains(&token) && rest.starts_with('(');
        if !bracketed
            && !function_call
            && !paging
            && !follows_bare_as(upper, start)
            && !tsql_allowed_use(token, rest)
        {
            return false;
        }
    }
    true
}

/// True when `token` starts with a digit and ends in a statement word. T-SQL
/// ends a number literal where a letter begins, so `SELECT 1DELETE FROM t`
/// runs `SELECT 1` and then `DELETE FROM t`, and `0xFFUPDATE` reads as `0xFF`
/// and `UPDATE`.
fn hidden_starter(token: &str, extra: &[&str]) -> bool {
    let bytes = token.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_digit)
        && (1..bytes.len())
            .filter(|&index| bytes[index].is_ascii_alphabetic())
            .any(|index| {
                let suffix = &token[index..];
                STATEMENT_STARTERS.contains(&suffix) || extra.contains(&suffix)
            })
}

/// Whether the statement word `token`, followed by `rest`, is one a read batch
/// may use.
fn tsql_allowed_use(token: &str, rest: &str) -> bool {
    let next = leading_word(rest);
    let after = leading_word(rest[next.len()..].trim_start());
    match token {
        "SET" => {
            rest.starts_with('@')
                || TSQL_SET_OPTIONS.contains(&next)
                || (next == "TRANSACTION" && after == "ISOLATION")
                || (next == "STATISTICS" && ["IO", "TIME", "XML", "PROFILE"].contains(&after))
        }
        "DECLARE" => rest.starts_with('@'),
        "BEGIN" => !["TRAN", "TRANSACTION", "DISTRIBUTED", "DIALOG"].contains(&next),
        "WAITFOR" => ["DELAY", "TIME"].contains(&next),
        "USE" => {
            ["INDEX", "KEY", "PLAN"].contains(&next)
                || (next == "HINT" && rest[next.len()..].trim_start().starts_with('('))
        }
        _ => false,
    }
}

/// Functions that change server, session or sequence state even when called
/// from a `SELECT`: backend signals, settings, sequences, advisory locks, large
/// objects, files, replication, notifications and remote SQL.
const SIDE_EFFECT_FUNCTIONS: &[&str] = &[
    "PG_CANCEL_BACKEND",
    "PG_TERMINATE_BACKEND",
    "PG_RELOAD_CONF",
    "PG_ROTATE_LOGFILE",
    "PG_SWITCH_WAL",
    "PG_SWITCH_XLOG",
    "PG_CREATE_RESTORE_POINT",
    "PG_PROMOTE",
    "PG_WAL_REPLAY_PAUSE",
    "PG_WAL_REPLAY_RESUME",
    "PG_BACKUP_START",
    "PG_BACKUP_STOP",
    "PG_START_BACKUP",
    "PG_STOP_BACKUP",
    "PG_LOG_BACKEND_MEMORY_CONTEXTS",
    "PG_NOTIFY",
    "PG_LOGICAL_EMIT_MESSAGE",
    "PG_IMPORT_SYSTEM_COLLATIONS",
    "PG_FILE_WRITE",
    "PG_FILE_RENAME",
    "PG_FILE_UNLINK",
    "PG_FILE_SYNC",
    "PG_CREATE_PHYSICAL_REPLICATION_SLOT",
    "PG_CREATE_LOGICAL_REPLICATION_SLOT",
    "PG_DROP_REPLICATION_SLOT",
    "PG_COPY_PHYSICAL_REPLICATION_SLOT",
    "PG_COPY_LOGICAL_REPLICATION_SLOT",
    "PG_REPLICATION_SLOT_ADVANCE",
    "PG_LOGICAL_SLOT_GET_CHANGES",
    "PG_LOGICAL_SLOT_GET_BINARY_CHANGES",
    "SET_CONFIG",
    "NEXTVAL",
    "SETVAL",
    "LO_IMPORT",
    "LO_EXPORT",
    "LO_UNLINK",
    "LO_CREATE",
    "LO_CREAT",
    "LO_FROM_BYTEA",
    "LO_PUT",
    "LO_TRUNCATE",
    "LO_TRUNCATE64",
    "LOWRITE",
    "GET_LOCK",
    "RELEASE_LOCK",
    "RELEASE_ALL_LOCKS",
];

const SIDE_EFFECT_FUNCTION_PREFIXES: &[&str] = &[
    "PG_ADVISORY_",
    "PG_TRY_ADVISORY_",
    "PG_STAT_RESET",
    "PG_STAT_STATEMENTS_RESET",
    "PG_REPLICATION_ORIGIN_",
    "DBLINK",
];

/// True when `sql` calls a function from [`SIDE_EFFECT_FUNCTIONS`], also as a
/// schema-qualified or quoted name, or takes a sequence value with
/// `NEXT VALUE FOR`. Quoted identifiers keep their name here, and a Unicode
/// escaped identifier (`U&"…"`) fails closed because its name cannot be read
/// without decoding it. Every quoting interpretation is checked and any match
/// counts, so a call hidden by one dialect's quoting rules is still found.
fn calls_side_effect_function(sql: &str, variants: &[StripOptions]) -> bool {
    if sql.to_ascii_uppercase().contains("U&\"") {
        return true;
    }
    variants.iter().any(|options| {
        let kept = strip_with(
            sql,
            StripOptions {
                keep_identifiers: true,
                ..*options
            },
        )
        .to_ascii_uppercase();
        has_side_effect_call(&kept) || has_next_value_for(&kept)
    })
}

fn has_side_effect_call(upper: &str) -> bool {
    let bytes = upper.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut cursor = 0;
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
        let listed = SIDE_EFFECT_FUNCTIONS.contains(&token)
            || SIDE_EFFECT_FUNCTION_PREFIXES
                .iter()
                .any(|prefix| token.starts_with(prefix));
        if listed && upper[cursor..].trim_start().starts_with('(') {
            return true;
        }
    }
    false
}

fn has_next_value_for(upper: &str) -> bool {
    let mut words = upper
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty());
    let mut previous = ["", ""];
    words.any(|word| {
        let found = previous == ["NEXT", "VALUE"] && word == "FOR";
        previous = [previous[1], word];
        found
    })
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
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE" | "KILL" => "write",
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
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "UPSERT",
    "REPLACE",
    "TRUNCATE",
    "CREATE",
    "ALTER",
    "DROP",
    "RENAME",
    "GRANT",
    "REVOKE",
    "DENY",
    "EXEC",
    "EXECUTE",
    "CALL",
    "DECLARE",
    "SET",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "SAVE",
    "USE",
    "BACKUP",
    "RESTORE",
    "DBCC",
    "KILL",
    "SHUTDOWN",
    "RECONFIGURE",
    "BULK",
    "OPENROWSET",
    "OPENDATASOURCE",
    "OPENQUERY",
    "WAITFOR",
    "GO",
    "COPY",
    "VACUUM",
    "LOCK",
    "LOAD",
    "ATTACH",
    "DETACH",
    "PRAGMA",
    "HANDLER",
    "IMPORT",
    "INTO",
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
        if hidden_starter(token, &[]) {
            return true;
        }
        if !STATEMENT_STARTERS.contains(&token) {
            continue;
        }
        let rest = upper[cursor..].trim_start();
        let bracketed = start > 0 && bytes[start - 1] == b'[' && bytes.get(cursor) == Some(&b']');
        let function_call = FUNCTION_LIKE_STARTERS.contains(&token) && rest.starts_with('(');
        let show_create = token == "CREATE" && skip_leading_create && was_first;
        let index_hint = token == "USE" && (rest.starts_with("INDEX") || rest.starts_with("KEY"));
        if bracketed || function_call || show_create || index_hint || follows_bare_as(upper, start)
        {
            continue;
        }
        return true;
    }
    false
}

/// True when the word starting at `start` follows a bare `AS` and whitespace.
/// Only an alias can stand there, so the word cannot begin a statement: an
/// engine that does not accept it as an alias rejects the whole batch. A
/// quoted, bracketed or qualified `AS` is an identifier and does not count.
fn follows_bare_as(upper: &str, start: usize) -> bool {
    let before = &upper[..start];
    let head = before.trim_end_matches(|c: char| c.is_ascii_whitespace());
    if head.len() == before.len() {
        return false;
    }
    head.strip_suffix("AS").is_some_and(|prefix| {
        !prefix.ends_with(|c: char| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '[' | '"' | '`' | '.' | '@' | '$' | '#')
        })
    })
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
    /// Keep the letters, digits and `_` of quoted identifiers instead of
    /// blanking them, so a quoted function name stays visible.
    keep_identifiers: bool,
    /// `/* /* … */ */` nests, as in PostgreSQL and SQL Server.
    nested_comments: bool,
}
impl StripOptions {
    const STANDARD: Self = Self {
        backslash_escapes: false,
        brackets: false,
        dollar_quotes: false,
        keep_identifiers: false,
        nested_comments: false,
    };
}

fn identifier_byte(byte: u8, options: StripOptions) -> u8 {
    if options.keep_identifiers && (byte.is_ascii_alphanumeric() || byte == b'_') {
        byte
    } else {
        b' '
    }
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
                    out.push(identifier_byte(bytes[i], options));
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
        // Block comment; it nests only under `nested_comments`.
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let mut depth = 0usize;
            while i < bytes.len() {
                let opens = bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*');
                if opens && (depth == 0 || options.nested_comments) {
                    depth += 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
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
                out.push(identifier_byte(bytes[i], options));
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
                out.push(identifier_byte(bytes[i], options));
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
