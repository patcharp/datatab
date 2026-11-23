use datatab_policy::ExecutionClassification;

use crate::QueryLanguage;

use super::language_service::{
    DangerousQueryKind, LanguageService, classify_query_for_language_with_service,
    detect_dangerous_single,
};

/// Lexical rules of one SQL dialect. Classification lexes the query once per
/// dialect and keeps a result that is never less restricted than any dialect's
/// reading, so a keyword counts as code when any supported dialect would read it
/// as code. This closes gaps where one dialect sees a string or comment that
/// another dialect executes.
#[derive(Clone, Copy, Debug)]
struct LexRules {
    nested_block_comments: bool,
    dash_comment_requires_space: bool,
    hash_line_comments: bool,
    executable_comments: bool,
    backslash_escapes: bool,
    escape_string_prefix: bool,
    dollar_quotes: bool,
    bracket_identifiers: bool,
}

const POSTGRES_RULES: LexRules = LexRules {
    nested_block_comments: true,
    dash_comment_requires_space: false,
    hash_line_comments: false,
    executable_comments: false,
    backslash_escapes: false,
    escape_string_prefix: true,
    dollar_quotes: true,
    bracket_identifiers: false,
};

const MYSQL_RULES: LexRules = LexRules {
    nested_block_comments: false,
    dash_comment_requires_space: true,
    hash_line_comments: true,
    executable_comments: true,
    backslash_escapes: true,
    escape_string_prefix: false,
    dollar_quotes: false,
    bracket_identifiers: false,
};

const SQLITE_RULES: LexRules = LexRules {
    nested_block_comments: false,
    dash_comment_requires_space: false,
    hash_line_comments: false,
    executable_comments: false,
    backslash_escapes: false,
    escape_string_prefix: false,
    dollar_quotes: false,
    bracket_identifiers: true,
};

const TSQL_RULES: LexRules = LexRules {
    nested_block_comments: true,
    dash_comment_requires_space: false,
    hash_line_comments: false,
    executable_comments: false,
    backslash_escapes: false,
    escape_string_prefix: false,
    dollar_quotes: false,
    bracket_identifiers: true,
};

const DIALECT_RULES: [LexRules; 4] = [POSTGRES_RULES, MYSQL_RULES, SQLITE_RULES, TSQL_RULES];

/// Deepest procedural body a `DO` is re-read through. Each level holds a
/// strict substring of the one above it, so the recursion ends on its own; the
/// cap keeps a crafted buffer from spending the stack to get there. A body past
/// it is still read for the statements it holds, just not opened again.
const MAX_BODY_DEPTH: usize = 8;

/// Keywords a procedural body puts before the statement it opens, so a row
/// change after one starts a statement of its own even though the splitter
/// cut no `;` in front of it.
const BODY_STATEMENT_LEADERS: [&str; 11] = [
    "BEGIN",
    "THEN",
    "ELSE",
    "ELSEIF",
    "LOOP",
    "REPEAT",
    "WHILE",
    "DO",
    "EXCEPTION",
    "TRY",
    "CATCH",
];

/// First words that make the tokens after a body keyword a statement of their
/// own. Only a keyword counts: any other word in that place — a column name in
/// a `CASE` arm, say — leaves the chunk as it was.
const BODY_STATEMENT_STARTS: [&str; 11] = [
    "DELETE", "UPDATE", "INSERT", "MERGE", "REPLACE", "TRUNCATE", "DROP", "ALTER", "CREATE",
    "GRANT", "REVOKE",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanState {
    Normal,
    LineComment,
    BlockComment {
        depth: usize,
    },
    Quoted {
        close: char,
        backslash_escapes: bool,
        /// Char index where the literal's content starts, so the contents can
        /// be kept for a `DO '...'` body (#507).
        content_start: usize,
    },
    DollarQuoted {
        tag_start: usize,
        tag_len: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenKind {
    /// An unquoted word, uppercased.
    Word(String),
    /// A string literal or quoted identifier. Its content is code only where
    /// a `DO` runs it as a body (#507).
    Quoted,
    Symbol(char),
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    /// Contents of a dollar-quoted token (`$$ ... $$`), which PostgreSQL's
    /// `DO` and `EXECUTE` re-read as code. `None` for every other token.
    dollar_body: Option<String>,
    /// Contents of a single-quoted string literal, which a PostgreSQL
    /// `DO '...'` re-reads as code the same way (#507). `None` for every other
    /// token, including a quoted identifier.
    quoted_body: Option<String>,
}

impl Token {
    fn new(kind: TokenKind) -> Self {
        Self {
            kind,
            dollar_body: None,
            quoted_body: None,
        }
    }

    fn word(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Word(word) => Some(word),
            _ => None,
        }
    }

    fn is_symbol(&self, symbol: char) -> bool {
        self.kind == TokenKind::Symbol(symbol)
    }
}

/// Classify a SQL query's execution impact for MCP/policy governance and for
/// editor auto-refresh.
///
/// This is a lexical heuristic, not a security boundary on its own. It finds
/// writes hidden in data-modifying CTEs, `EXPLAIN ANALYZE`, `INTO` clauses,
/// locking reads and multi-statement batches, but it cannot see side effects
/// of function calls such as `pg_terminate_backend`, `setval` or
/// `dblink_exec`. Enforcing read-only execution at the database is tracked
/// separately.
///
/// A `DELETE` or `UPDATE` with no restricting `WHERE` is `Destructive`, like
/// `DROP`: policy and approval read the class alone, so it must not pass as a
/// keyed row edit (#489).
pub fn classify_sql_execution(sql: &str) -> ExecutionClassification {
    DIALECT_RULES
        .iter()
        .map(|rules| classify_with_rules(sql, rules))
        .fold(
            ExecutionClassification::Metadata,
            ExecutionClassification::compose,
        )
}

/// Classify a query's execution impact for MCP/policy governance.
///
/// Pass the live connection's `Connection::language_service()` as `service`
/// whenever one is available, so driver-owned dialects (`Cql`, `InfluxQuery`,
/// `Flux`, `Custom(_)`, `MongoQuery`, and the CloudWatch/OpenSearch trio) are
/// classified by the driver instead of a conservative default. `None`
/// preserves the language-keyed fallback classification for those dialects —
/// for `MongoQuery` that fallback is the pre-existing core text heuristic
/// (`classify_mongo_query`), unchanged. `Sql` and `RedisCommands` always use
/// the shared core heuristic regardless of `service`.
pub fn classify_query_for_governance(
    query_language: &QueryLanguage,
    query: &str,
    service: Option<&dyn LanguageService>,
) -> ExecutionClassification {
    classify_query_for_language_with_service(query_language, query, service)
}

pub fn is_safe_read_query(sql: &str) -> bool {
    let has_statement_in_every_dialect = DIALECT_RULES
        .iter()
        .all(|rules| !tokenize(sql, rules).is_empty());

    has_statement_in_every_dialect
        && matches!(
            classify_sql_execution(sql),
            ExecutionClassification::Read | ExecutionClassification::Metadata
        )
}

/// The dangerous-kind scan, over the same statements the class reads.
///
/// The hard-confirm dialog names a pattern, so it has to read what the class
/// reads: the `;`-delimited chunks and every statement a procedural body or a
/// data-modifying CTE opens inside one, lexed by the same rules and unioned
/// across the same dialects. Reading a body on one side alone left
/// `BEGIN DELETE FROM orders; END` refused by a `write`-only policy while the
/// dialog named nothing (#497, #507), and left `WITH d AS (DELETE FROM t)
/// SELECT 1` the same way (#513). A statement an `EXPLAIN ANALYZE` runs is one
/// of them: the class reads the target it wraps, so the scan does too (#515).
///
/// The public entry point is `language_service::detect_dangerous_sql`; the text
/// rules for one statement stay there (`detect_dangerous_single`).
pub(crate) fn detect_dangerous_sql(query: &str) -> Option<DangerousQueryKind> {
    DIALECT_RULES
        .iter()
        .filter_map(|rules| detect_dangerous_at(query, rules, 0))
        .fold(None, worse_kind)
}

/// The worse of two kinds the per-dialect readings found. A named pattern beats
/// `Script`, which only says that some statement of the buffer is dangerous, so
/// a dialect that splits a body it cannot lex cannot hide the pattern another
/// dialect names.
fn worse_kind(
    current: Option<DangerousQueryKind>,
    found: DangerousQueryKind,
) -> Option<DangerousQueryKind> {
    match current {
        None | Some(DangerousQueryKind::Script) => Some(found),
        Some(current) => Some(current),
    }
}

fn detect_dangerous_at(sql: &str, rules: &LexRules, depth: usize) -> Option<DangerousQueryKind> {
    let tokens = tokenize(sql, rules);

    let chunks: Vec<&[Token]> = tokens
        .split(|token| token.is_symbol(';'))
        .filter(|chunk| !chunk.is_empty())
        .collect();

    if chunks.is_empty() {
        return None;
    }

    let is_buffer = chunks.len() > 1;

    // A statement of the buffer itself is what `Script` names, as before; a
    // body's inner `;` is the body's, so the pattern the body holds is named
    // instead.
    for chunk in &chunks {
        let kind = detect_dangerous_single(&statement_text(chunk))
            .or_else(|| explain_target_danger(chunk));

        if let Some(kind) = kind {
            return Some(if is_buffer {
                DangerousQueryKind::Script
            } else {
                kind
            });
        }
    }

    chunks
        .iter()
        .find_map(|chunk| body_danger(chunk, rules, depth))
}

/// The dangerous kind of the statement an executing `EXPLAIN` runs.
///
/// `EXPLAIN ANALYZE` runs the statement it wraps and the class reads it that
/// way, but the chunk's own text starts with `EXPLAIN` and matches no text
/// rule, so a policy refused the statement while the dialog named nothing
/// (#515). The target `explain_options` splits out is read with the same rules
/// a chunk gets, so the kind follows the class. A plan or a `DESCRIBE` runs
/// nothing and names nothing, as #513 pinned.
///
/// A target whose text rules find no pattern keeps the reading it had: a
/// data-modifying CTE is named by `body_danger` (#513), and a `WHERE` only a
/// subquery holds is left to the class (#489). Neither ladder guesses where
/// the text cannot separate the statement.
fn explain_target_danger(chunk: &[Token]) -> Option<DangerousQueryKind> {
    let leading = skip_leading_symbols(chunk);

    let explains = leading
        .first()
        .and_then(Token::word)
        .is_some_and(|word| matches!(word, "EXPLAIN" | "DESC" | "DESCRIBE"));

    if !explains || !explain_executes(chunk) {
        return None;
    }

    let (_, target) = explain_options(leading.get(1..).unwrap_or_default());

    detect_dangerous_single(&statement_text(target))
}

/// The first dangerous kind a body inside one chunk names.
///
/// Each statement a body opens is read with the same text rules a chunk gets,
/// cut where its enclosing parenthesis closes, and a body a statement runs
/// (`DO '...'`, `DO $$ ... $$`, `EXECUTE $tag$ ... $tag$`) is opened again, up
/// to the same cap the class uses. A statement that only plans what it names
/// runs no body, so it names no pattern (#513).
fn body_danger(chunk: &[Token], rules: &LexRules, depth: usize) -> Option<DangerousQueryKind> {
    if !explain_executes(chunk) {
        return None;
    }

    for start in body_statement_starts(chunk) {
        let statement = chunk.get(start..).unwrap_or_default();

        if let Some(kind) = detect_dangerous_single(&body_statement_text(statement)) {
            return Some(kind);
        }
    }

    let body = executed_body(chunk)?;

    if depth >= MAX_BODY_DEPTH {
        // The class cannot open the body at the cap either and keeps its
        // conservative class, so the scan keeps a hard confirm rather than
        // naming nothing (#497).
        return Some(DangerousQueryKind::Script);
    }

    detect_dangerous_at(body, rules, depth + 1)
}

/// The text of one statement, rebuilt from its tokens for the text rules
/// `detect_dangerous_single` applies.
///
/// A quoted token becomes `''`: what it holds is not code to this scan, and the
/// one place it is code — a `DO` body — is opened through `executed_body`.
fn statement_text(statement: &[Token]) -> String {
    let mut text = String::new();

    for token in statement {
        if !text.is_empty() {
            text.push(' ');
        }

        match &token.kind {
            TokenKind::Word(word) => text.push_str(word),
            TokenKind::Quoted => text.push_str("''"),
            TokenKind::Symbol(symbol) => text.push(*symbol),
        }
    }

    text
}

/// The text of one statement `body_statement_starts` opens, cut where the
/// parenthesis that encloses it closes.
///
/// A data-modifying CTE is one statement inside a longer chunk, so the `WHERE`
/// of the statement that follows it is not this statement's filter: in
/// `WITH d AS (DELETE FROM t) SELECT * FROM d WHERE x = 1` the trailing filter
/// must not hide the body's whole-table removal (#513). A `WHERE` inside the
/// statement's own parentheses is kept, as it is for a `;`-chunk.
fn body_statement_text(statement: &[Token]) -> String {
    let mut depth = 0usize;
    let mut end = statement.len();

    for (index, token) in statement.iter().enumerate() {
        if token.is_symbol('(') {
            depth += 1;
        } else if token.is_symbol(')') {
            if depth == 0 {
                end = index;
                break;
            }

            depth -= 1;
        }
    }

    statement_text(statement.get(..end).unwrap_or_default())
}

/// Whether `sql` holds a compound block: the `BEGIN ... END` body of a
/// procedure, function or trigger, an anonymous block, `BEGIN TRY` or
/// `BEGIN ATOMIC`. The statement splitter cuts such a block at its inner `;`,
/// so its pieces are not standalone statements.
///
/// A `BEGIN` that opens a transaction is not a block, nor one used as a
/// column name (MySQL allows `begin` unquoted). A block counts only when every
/// dialect sees it outside comments, strings and quoted bodies, so a
/// PostgreSQL `DO $$ BEGIN ... END $$` body is not one.
pub fn contains_compound_block(sql: &str) -> bool {
    DIALECT_RULES.iter().all(|rules| {
        let tokens = tokenize(sql, rules);
        tokens.iter().enumerate().any(|(index, token)| {
            let previous = index.checked_sub(1).and_then(|index| tokens.get(index));
            let next = tokens.get(index + 1);

            token.word() == Some("BEGIN")
                && !begin_opens_transaction(next)
                && !begin_is_identifier(previous, next)
        })
    })
}

/// Whether a `BEGIN` between `previous` and `next` is a column or table name:
/// qualified (`t.begin`), followed by a symbol (`begin,`, `begin = 1`), or
/// followed by a word that continues an expression or clause. Anything else
/// is taken as a block, which only makes the editor run the whole buffer.
fn begin_is_identifier(previous: Option<&Token>, next: Option<&Token>) -> bool {
    if previous.is_some_and(|token| token.is_symbol('.')) {
        return true;
    }

    match next {
        Some(Token {
            kind: TokenKind::Symbol(symbol),
            ..
        }) => *symbol != ';',
        Some(token) => matches!(
            token.word(),
            Some(
                "FROM"
                    | "AS"
                    | "WHERE"
                    | "AND"
                    | "OR"
                    | "IS"
                    | "IN"
                    | "LIKE"
                    | "BETWEEN"
                    | "ORDER"
                    | "GROUP"
                    | "HAVING"
                    | "LIMIT"
                    | "ON"
                    | "JOIN"
                    | "INTO"
                    | "UNION"
                    | "ASC"
                    | "DESC"
                    | "THEN"
                    | "ELSE"
                    | "END"
            )
        ),
        None => false,
    }
}

/// Whether a `BEGIN` followed by `next` opens a transaction (`BEGIN;`,
/// `BEGIN TRANSACTION`, `BEGIN ISOLATION LEVEL ...`, SQLite's
/// `BEGIN IMMEDIATE`) rather than a compound block.
fn begin_opens_transaction(next: Option<&Token>) -> bool {
    match next {
        None => true,
        Some(token) if token.is_symbol(';') => true,
        Some(token) => matches!(
            token.word(),
            Some(
                "TRANSACTION"
                    | "TRAN"
                    | "WORK"
                    | "ISOLATION"
                    | "READ"
                    | "DEFERRED"
                    | "IMMEDIATE"
                    | "EXCLUSIVE"
                    | "DISTRIBUTED"
            )
        ),
    }
}

fn classify_with_rules(sql: &str, rules: &LexRules) -> ExecutionClassification {
    classify_with_rules_at(sql, rules, 0)
}

fn classify_with_rules_at(sql: &str, rules: &LexRules, depth: usize) -> ExecutionClassification {
    let tokens = tokenize(sql, rules);

    let statements: Vec<&[Token]> = tokens
        .split(|token| token.is_symbol(';'))
        .filter(|statement| !statement.is_empty())
        .collect();

    // `compose`, not `max`: the buffer must never be less restricted than one
    // of its own statements, so a destructive statement is not folded down to
    // the admin class beside it.
    let classification = statements
        .iter()
        .map(|statement| classify_chunk(statement, rules, depth))
        .fold(
            ExecutionClassification::Metadata,
            ExecutionClassification::compose,
        );

    if statements.len() > 1 {
        classification.compose(ExecutionClassification::Write)
    } else {
        classification
    }
}

/// Classifies one `;`-delimited chunk, together with the statements any
/// procedural body in it opens.
///
/// The splitter cuts a body at its inner `;`, so the leading word alone would
/// leave a row change inside the body unread: `BEGIN DELETE FROM t; END` reads
/// as `BEGIN` and a `DO $$ ... $$` body is a string literal to the lexer.
/// Both are read here by the same classifier, and the worst class wins
/// (#497).
fn classify_chunk(statement: &[Token], rules: &LexRules, depth: usize) -> ExecutionClassification {
    // A statement that only plans what it names runs none of it, so the bodies
    // inside it are not code either (#513).
    let own = if explain_executes(statement) {
        classify_statement(statement).compose(classify_body_statements(statement))
    } else {
        classify_statement(statement)
    };

    if depth >= MAX_BODY_DEPTH {
        // The body is not opened again, so its worst statement is unknown. A
        // `DO` is the one shape that runs its string as code, so a body at the
        // cap keeps the conservative class rather than a plain `Write`.
        return match executed_body(statement) {
            Some(_) => own.compose(ExecutionClassification::Destructive),
            None => own,
        };
    }

    own.compose(classify_executed_body(statement, rules, depth))
}

/// Classifies each statement that a body opens inside one chunk, so
/// `BEGIN DELETE FROM t` and `IF ... THEN DELETE FROM t` are read as the row
/// change they hold, and a data-modifying CTE's `DELETE`/`UPDATE` is read as
/// the statement it is (#513).
///
/// A body puts a statement after its keywords, not after a `;` the splitter
/// could use: `BEGIN`, `THEN` (an `IF` or `WHEN` arm), `ELSE`, `ELSEIF`,
/// `LOOP`, `REPEAT`, `WHILE`, `DO`, `EXCEPTION`, `TRY` and `CATCH`. Those
/// keywords are the same in every dialect that has them, so this reads a body
/// without deciding which dialect wrote it — and without telling a body from a
/// transaction block, which the conservative union makes unnecessary: a
/// `DELETE` right after `BEGIN` is read the same either way.
fn classify_body_statements(statement: &[Token]) -> ExecutionClassification {
    let mut classification = ExecutionClassification::Metadata;

    for index in body_statement_starts(statement) {
        classification = classification.compose(classify_statement(
            statement.get(index..).unwrap_or_default(),
        ));
    }

    classification
}

/// The index of each statement a body opens inside one `;`-chunk, in the order
/// the body writes them. The class and the dangerous-kind scan both read this,
/// so the two never see a different set of statements (#507).
///
/// A data-modifying CTE is one of those statements: it puts a `DELETE` or an
/// `UPDATE` right after `(` or `)`, with no keyword in front of it, and the
/// class reads that position through `starts_nested_statement` (#513).
fn body_statement_starts(statement: &[Token]) -> impl Iterator<Item = usize> + '_ {
    statement
        .iter()
        .enumerate()
        .filter_map(move |(index, token)| {
            let word = token.word()?;

            if !BODY_STATEMENT_STARTS.contains(&word)
                || body_start_is_a_function_call(statement, index)
                || !opens_body_statement(statement, index)
            {
                return None;
            }

            Some(index)
        })
}

/// Whether the start word at `index` is a function call rather than a
/// statement: `THEN truncate(score, 2)` in a `CASE` arm names a function, so a
/// plain read must not become a write because of it. The same guard
/// `classify_read_body` applies to a write keyword.
fn body_start_is_a_function_call(statement: &[Token], index: usize) -> bool {
    statement
        .get(index + 1)
        .is_some_and(|next| next.is_symbol('('))
}

/// Whether the start word at `index` begins a statement a body opens.
///
/// The keyword before it is the leader, with two exceptions where the same word
/// is a name elsewhere in the chunk, which a name-only check would get wrong: a
/// `MERGE`'s `THEN` arm is an action guarded by the `ON` clause, so only a
/// `WHEN MATCHED` or a `WHEN NOT MATCHED` in front of it makes it one; and an
/// upsert's `ON CONFLICT ... DO UPDATE` is an action of the `INSERT`, so only a
/// `DO` with an `ON CONFLICT` in front of it is one. A name spelled like either
/// keyword leaves the row change in the arm to be read on its own. A
/// `DELETE`/`UPDATE` with no leader at all is a data-modifying CTE, which puts
/// the statement right after a parenthesis (#513).
fn opens_body_statement(statement: &[Token], index: usize) -> bool {
    let Some(leader_index) = index.checked_sub(1) else {
        return false;
    };

    let leader = statement.get(leader_index).and_then(Token::word);

    match leader {
        Some("THEN") => !merge_action_before(statement, leader_index),
        Some("DO") => !upsert_action_before(statement, leader_index),
        Some(word) if BODY_STATEMENT_LEADERS.contains(&word) => true,
        // The leader is a `(` or `)` symbol, so this is a statement inside
        // parentheses and not one a keyword opens (#513).
        None if starts_a_cte_body(statement, index) => true,
        // T-SQL puts its statement after the condition, with no `DO` or `THEN`
        // in between: `WHILE 1 = 1 DELETE FROM t`, and `IF @x = 1 DELETE FROM
        // t` with no `BEGIN` (#507). MySQL writes a `DO` there and PostgreSQL
        // a `LOOP`, both leaders of their own, so only these two forms walk
        // over a condition.
        _ => condition_holds(statement, index, "WHILE") || condition_holds(statement, index, "IF"),
    }
}

/// Whether the `THEN` before `index` closes a `MERGE` action rather than an
/// `IF` arm: `WHEN MATCHED THEN`, `WHEN NOT MATCHED [BY TARGET] THEN`. The
/// `MATCHED` must follow a `WHEN`, or follow a `NOT` that itself follows the
/// `WHEN`, so `IF r.matched THEN` and `IF NOT matched THEN` name a column and
/// the row change in that arm is still read.
fn merge_action_before(statement: &[Token], index: usize) -> bool {
    let Some(matched) = keyword_before(statement, index, "MATCHED") else {
        return false;
    };

    match word_before_at(statement, matched, 1) {
        Some("WHEN") => true,
        Some("NOT") => word_before_at(statement, matched, 2) == Some("WHEN"),
        _ => false,
    }
}

/// Whether the `DO` before `index` is the action of an upsert:
/// `ON CONFLICT (id) DO UPDATE`, `ON CONFLICT ON CONSTRAINT c DO NOTHING`. The
/// `CONFLICT` must follow the `ON` that opens the clause, so a condition named
/// `conflict` leaves the `DO` statement to be read.
fn upsert_action_before(statement: &[Token], index: usize) -> bool {
    keyword_before(statement, index, "CONFLICT")
        .is_some_and(|conflict| word_before_at(statement, conflict, 1) == Some("ON"))
}

/// The index of `keyword` before `index` in the same clause, or `None` when a
/// leader or a start word comes first: the scan covers the clause that owns the
/// word and no further.
fn keyword_before(statement: &[Token], index: usize, keyword: &str) -> Option<usize> {
    for (candidate, token) in statement.iter().take(index).enumerate().rev() {
        match token.word() {
            Some(word) if word == keyword => return Some(candidate),
            Some(word)
                if BODY_STATEMENT_LEADERS.contains(&word)
                    || BODY_STATEMENT_STARTS.contains(&word) =>
            {
                return None;
            }
            _ => {}
        }
    }

    None
}

/// The word `offset` tokens before `index`, when that token is a word.
fn word_before_at(statement: &[Token], index: usize, offset: usize) -> Option<&str> {
    index
        .checked_sub(offset)
        .and_then(|earlier| statement.get(earlier))
        .and_then(Token::word)
}

/// Whether the statement at `index` is the one a `WHILE` or an `IF` states
/// after its condition, as T-SQL writes it: `WHILE 1 = 1 DELETE FROM t`,
/// `IF @x = 1 DELETE FROM t`.
///
/// The keyword must sit where a statement starts — the chunk's first word, or
/// after a body leader — so a column named `while` in a select list or a MySQL
/// `IF(cond, a, b)` call behind `AND` is a name, and the read stays a read.
fn condition_holds(statement: &[Token], index: usize, keyword: &str) -> bool {
    for (candidate, token) in statement.iter().take(index).enumerate().rev() {
        match token.word() {
            Some(word) if word == keyword => return starts_a_statement(statement, candidate),
            Some(word)
                if BODY_STATEMENT_LEADERS.contains(&word)
                    || BODY_STATEMENT_STARTS.contains(&word) =>
            {
                return false;
            }
            _ => {}
        }
    }

    false
}

/// Whether the token at `index` sits where a statement starts: it is the
/// chunk's first word, or it follows a body leader.
fn starts_a_statement(statement: &[Token], index: usize) -> bool {
    let is_first_word = statement
        .iter()
        .position(|token| !matches!(token.kind, TokenKind::Symbol(_)))
        == Some(index);

    is_first_word
        || index
            .checked_sub(1)
            .and_then(|previous| statement.get(previous))
            .and_then(Token::word)
            .is_some_and(|word| BODY_STATEMENT_LEADERS.contains(&word))
}

/// Classifies the body of a statement that runs a string as code.
///
/// A `DO` body is a string constant — `$$ ... $$`, `$tag$ ... $tag$`, or an
/// ordinary single-quoted `'...'`, which PostgreSQL runs the same way — so the
/// lexer keeps it opaque, and a row change inside it would otherwise be
/// invisible: `DO $$ BEGIN DELETE FROM t; END $$` reads as `DO`, class `Write`
/// (#497, #507). A PostgreSQL `EXECUTE $tag$ ... $tag$` runs its string too.
/// Nothing else does: a `CREATE FUNCTION ... AS $$ ... $$` defines a body
/// without running it, so it keeps the admin class its `CREATE` has.
///
/// A `BEGIN ... END` body needs none of that: it is unquoted code, so
/// `body_statement_starts` reads the statements it opens whether or not a
/// `CREATE` in front defines it. That is why a MySQL or T-SQL
/// `CREATE PROCEDURE ... BEGIN DELETE FROM t; END` is read — and hard-confirms
/// in the dialog as the class rates it (#507) — while a PostgreSQL
/// `AS $$ ... $$` body stays a string that only `DO` and `EXECUTE` open.
fn classify_executed_body(
    statement: &[Token],
    rules: &LexRules,
    depth: usize,
) -> ExecutionClassification {
    let Some(body) = executed_body(statement) else {
        return ExecutionClassification::Metadata;
    };

    classify_with_rules_at(body, rules, depth + 1)
}

/// The body a statement runs as code, when it has one: the `DO` or `EXECUTE`
/// statement at the start of the tokens, followed by the string it runs.
fn executed_body(statement: &[Token]) -> Option<&str> {
    let leading = skip_leading_symbols(statement);

    let body = leading.get(1)?;

    match leading.first().and_then(Token::word) {
        // PostgreSQL accepts both a dollar-quoted and a single-quoted `DO`
        // body and runs whichever it gets.
        Some("DO") => body.dollar_body.as_deref().or(body.quoted_body.as_deref()),
        // Only the dollar-quoted form is read: `EXECUTE '...'` is a shape of
        // its own that #507 does not cover.
        Some("EXECUTE") => body.dollar_body.as_deref(),
        _ => None,
    }
}

fn classify_statement(tokens: &[Token]) -> ExecutionClassification {
    let statement = skip_leading_symbols(tokens);

    let Some((first, rest)) = statement.split_first() else {
        return ExecutionClassification::Write;
    };

    let Some(keyword) = first.word() else {
        return ExecutionClassification::Write;
    };

    match keyword {
        "EXPLAIN" | "DESC" | "DESCRIBE" => classify_explain(rest),
        "SHOW" => ExecutionClassification::Metadata,
        "SELECT" | "WITH" => classify_read_body(rest),
        _ => classify_leading_keyword(keyword, statement),
    }
}

/// Drops the symbols, such as opening parentheses, that precede a statement's
/// first word or quoted token.
fn skip_leading_symbols(tokens: &[Token]) -> &[Token] {
    let first_index = tokens
        .iter()
        .position(|token| !matches!(token.kind, TokenKind::Symbol(_)))
        .unwrap_or(tokens.len());

    tokens.get(first_index..).unwrap_or_default()
}

fn classify_leading_keyword(keyword: &str, statement: &[Token]) -> ExecutionClassification {
    match keyword {
        "DELETE" | "UPDATE" => classify_row_change(statement),
        "INSERT" | "MERGE" | "REPLACE" => ExecutionClassification::Write,
        "TRUNCATE" | "DROP" | "ALTER" => ExecutionClassification::Destructive,
        "GRANT" | "REVOKE" | "CREATE" | "SET" | "SYSTEM" | "KILL" | "ATTACH" | "OPTIMIZE"
        | "BACKUP" | "RESTORE" | "UNDROP" | "MOVE" => ExecutionClassification::Admin,
        "DETACH" | "RENAME" | "EXCHANGE" => ExecutionClassification::Destructive,
        _ => ExecutionClassification::Write,
    }
}

/// Classifies a `DELETE` or `UPDATE` by whether a restricting `WHERE` bounds
/// it: a keyed row edit stays `Write`, a statement that can touch every row is
/// `Destructive`.
///
/// Policy and approval read this class alone, so a whole-table removal must be
/// above `Write`; otherwise a policy that allows row edits runs
/// `DELETE FROM orders` with no dialog and no human (#489). The dangerous-kind
/// scan in `language_service::detect_dangerous_single` names the same two
/// shapes (`DeleteNoWhere`, `UpdateNoWhere`) for the UI's hard confirm, but
/// reads lowercased text there. This reads the token stream instead, so a
/// `WHERE` inside a comment or a string does not restrict anything, and a
/// `WHERE` in a `SET` subquery does not count as the statement's own filter.
fn classify_row_change(statement: &[Token]) -> ExecutionClassification {
    if row_change_is_restricted(statement) {
        ExecutionClassification::Write
    } else {
        ExecutionClassification::Destructive
    }
}

/// Whether a `DELETE`/`UPDATE` starting at `statement[0]` carries a `WHERE`
/// of its own: one at its parenthesis depth, not one nested in a subquery.
///
/// A `WHERE` that only sits inside parentheses — `UPDATE t SET a = (SELECT
/// max(x) FROM u WHERE y)` — filters that subquery, not the rows the
/// statement rewrites, so it does not restrict the statement.
fn row_change_is_restricted(statement: &[Token]) -> bool {
    let mut depth = 0usize;

    for token in statement.iter().skip(1) {
        if token.is_symbol('(') {
            depth += 1;
        } else if token.is_symbol(')') {
            if depth == 0 {
                break;
            }

            depth -= 1;
        } else if depth == 0 && token.word() == Some("WHERE") {
            return true;
        }
    }

    false
}

/// `EXPLAIN` only plans a statement, unless `ANALYZE` makes it execute the
/// statement, in which case the explained statement decides the class.
fn classify_explain(tokens: &[Token]) -> ExecutionClassification {
    let (analyze, target) = explain_options(tokens);

    if !analyze || target.is_empty() {
        return ExecutionClassification::Metadata;
    }

    // No dialect explains an EXPLAIN; refusing it also bounds the recursion to
    // one level, even when the nested EXPLAIN sits inside parentheses.
    let explains_an_explain = skip_leading_symbols(target)
        .first()
        .and_then(Token::word)
        .is_some_and(|word| matches!(word, "EXPLAIN" | "DESC" | "DESCRIBE"));

    if explains_an_explain {
        return ExecutionClassification::Write;
    }

    classify_statement(target)
}

/// Whether a statement runs the statement it names, so a body inside that
/// statement is code.
///
/// `EXPLAIN`, `DESC` and `DESCRIBE` only plan what follows them unless
/// `ANALYZE` asks the server to run it, and an `EXPLAIN` with nothing to plan
/// runs nothing. Every other statement runs. The class and the dangerous-kind
/// scan both skip the bodies of a statement that does not run:
/// `EXPLAIN WITH d AS (DELETE FROM t) SELECT 1` plans a removal that never
/// happens, so it names no pattern (#513).
fn explain_executes(statement: &[Token]) -> bool {
    let leading = skip_leading_symbols(statement);

    match leading.first().and_then(Token::word) {
        Some("EXPLAIN" | "DESC" | "DESCRIBE") => {
            let (analyze, target) = explain_options(leading.get(1..).unwrap_or_default());

            analyze && !target.is_empty()
        }
        _ => true,
    }
}

/// The options `EXPLAIN` takes before the statement it wraps, and the statement
/// itself: whether the option list asks it to run that statement, and the
/// tokens after the options.
///
/// A quoted name in the parenthesized option list counts as `ANALYZE`, because
/// PostgreSQL accepts `EXPLAIN ("analyze") ...`.
fn explain_options(tokens: &[Token]) -> (bool, &[Token]) {
    let mut analyze = false;
    let mut index = 0;

    if tokens.first().is_some_and(|token| token.is_symbol('(')) {
        let mut depth = 0usize;

        while let Some(token) = tokens.get(index) {
            index += 1;

            if token.is_symbol('(') {
                depth += 1;
            } else if token.is_symbol(')') {
                depth -= 1;

                if depth == 0 {
                    break;
                }
            } else if matches!(token.word(), Some("ANALYZE" | "ANALYSE"))
                || token.kind == TokenKind::Quoted
            {
                analyze = true;
            }
        }
    }

    while let Some(token) = tokens.get(index) {
        let is_option = match &token.kind {
            TokenKind::Word(word) => matches!(
                word.as_str(),
                "ANALYZE"
                    | "ANALYSE"
                    | "VERBOSE"
                    | "QUERY"
                    | "PLAN"
                    | "EXTENDED"
                    | "PARTITIONS"
                    | "FORMAT"
                    | "TREE"
                    | "JSON"
                    | "TRADITIONAL"
            ),
            TokenKind::Symbol('=') => true,
            _ => false,
        };

        if !is_option {
            break;
        }

        if matches!(token.word(), Some("ANALYZE" | "ANALYSE")) {
            analyze = true;
        }

        index += 1;
    }

    let target = tokens.get(index..).unwrap_or_default();

    (analyze, target)
}

/// Classifies the body of a `SELECT` or `WITH` statement: a write keyword
/// anywhere (for example a data-modifying CTE), an `INTO` clause or a locking
/// clause makes it a write. An unquoted identifier spelled like a write
/// keyword is over-classified on purpose.
fn classify_read_body(tokens: &[Token]) -> ExecutionClassification {
    let mut classification = ExecutionClassification::Read;

    for (index, token) in tokens.iter().enumerate() {
        let Some(word) = token.word() else {
            continue;
        };

        let previous = index
            .checked_sub(1)
            .and_then(|previous| tokens.get(previous));
        let next = tokens.get(index + 1);

        let qualifier = index
            .checked_sub(2)
            .and_then(|qualifier| tokens.get(qualifier));

        // `1.` is a numeric literal, so a word after it is not a qualified name.
        let is_qualified_name = previous.is_some_and(|previous| previous.is_symbol('.'))
            && qualifier.is_some_and(|qualifier| match &qualifier.kind {
                TokenKind::Word(word) => {
                    !word.starts_with(|character: char| character.is_ascii_digit())
                }
                TokenKind::Quoted => true,
                TokenKind::Symbol(_) => false,
            });
        let is_lone_bracketed_name = previous.is_some_and(|previous| previous.is_symbol('['))
            && next.is_some_and(|next| next.is_symbol(']'));

        if is_qualified_name || is_lone_bracketed_name {
            continue;
        }

        let is_function_call = next.is_some_and(|next| next.is_symbol('('));

        let found = match word {
            "REPLACE" | "INSERT" | "TRUNCATE" if is_function_call => None,
            // A `DELETE`/`UPDATE` that starts a statement of its own — a
            // data-modifying CTE, or a CTE-prefixed one — is classified as the
            // statement it is, so a whole-table removal inside a read body is
            // not folded down to a keyed edit (#489).
            "DELETE" | "UPDATE" if starts_nested_statement(tokens, index) => {
                Some(classify_row_change(tokens.get(index..).unwrap_or_default()))
            }
            "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "REPLACE" | "UPSERT" | "COPY" | "CALL"
            | "DO" | "LOCK" | "INTO" => Some(ExecutionClassification::Write),
            "TRUNCATE" | "DROP" | "ALTER" => Some(ExecutionClassification::Destructive),
            "CREATE" | "GRANT" | "REVOKE" => Some(ExecutionClassification::Admin),
            "FOR"
                if next
                    .and_then(Token::word)
                    .is_some_and(|next| matches!(next, "UPDATE" | "SHARE" | "NO" | "KEY")) =>
            {
                Some(ExecutionClassification::Write)
            }
            _ => None,
        };

        if let Some(found) = found {
            classification = classification.compose(found);
        }
    }

    classification
}

/// Whether the `DELETE` or `UPDATE` at `index` in a read body begins a
/// statement of its own.
///
/// A data-modifying CTE, or a `DELETE`/`UPDATE` prefixed by one, puts the
/// keyword directly after `(` or `)` — `WITH d AS (DELETE FROM t RETURNING *)`
/// or `WITH c AS (SELECT 1) DELETE FROM t`. Any other position leaves it a
/// word read as code at `Write`, as before: `FOR UPDATE`, a `MERGE` action
/// after `THEN`, or a column named `delete`.
fn starts_nested_statement(tokens: &[Token], index: usize) -> bool {
    index == 0
        || tokens
            .get(index - 1)
            .is_some_and(|previous| previous.is_symbol('(') || previous.is_symbol(')'))
}

/// Whether the `DELETE` or `UPDATE` at `index` opens a data-modifying CTE:
/// `WITH d AS (DELETE FROM t) SELECT 1`.
///
/// The class reads that statement through `starts_nested_statement`, so the
/// start finder `body_statement_starts` must see it too; otherwise the dialog
/// walks the chunk to its trailing `SELECT` and names no pattern while the
/// class refuses the body (#513). The chunk's own first word is left out: the
/// text rules already read the statement the chunk starts with.
fn starts_a_cte_body(statement: &[Token], index: usize) -> bool {
    index > 0
        && matches!(
            statement.get(index).and_then(Token::word),
            Some("DELETE" | "UPDATE")
        )
        && starts_nested_statement(statement, index)
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// The contents of a string literal that a PostgreSQL `DO '...'` runs as code
/// (#507).
///
/// `start` is the char index just after the opening quote and `end` the index
/// of the closing quote. A doubled quote is one quote inside the literal, and a
/// backslash escape (MySQL, or a PostgreSQL `E'...'`) loses its backslash, so
/// the body reads as the text the server would run.
fn quoted_body(
    chars: &[(usize, char)],
    start: usize,
    end: usize,
    close: char,
    backslash_escapes: bool,
) -> String {
    let mut body = String::new();
    let mut index = start;
    let char_at = |at: usize| chars.get(at).map(|&(_, character)| character);

    while index < end {
        let Some(character) = char_at(index) else {
            break;
        };

        if backslash_escapes && character == '\\' {
            if index + 1 < end
                && let Some(escaped) = char_at(index + 1)
            {
                body.push(escaped);
            }

            index += 2;
            continue;
        }

        if character == close && index + 1 < end && char_at(index + 1) == Some(close) {
            body.push(close);
            index += 2;
            continue;
        }

        body.push(character);
        index += 1;
    }

    body
}

/// Length in chars of a dollar-quote tag (`$$` or `$tag$`) starting at `start`.
fn dollar_tag_len(chars: &[(usize, char)], start: usize) -> Option<usize> {
    let mut index = start + 1;

    match chars.get(index).map(|&(_, character)| character) {
        Some('$') => return Some(2),
        Some(character) if character.is_alphabetic() || character == '_' => {}
        _ => return None,
    }

    while let Some(&(_, character)) = chars.get(index) {
        if character == '$' {
            return Some(index - start + 1);
        }

        if !(character.is_alphanumeric() || character == '_') {
            return None;
        }

        index += 1;
    }

    None
}

/// Splits `sql` into words and symbols outside comments, string literals and
/// quoted identifiers, following one dialect's lexical rules.
fn tokenize(sql: &str, rules: &LexRules) -> Vec<Token> {
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let char_at = |index: usize| chars.get(index).map(|&(_, character)| character);

    let mut tokens = Vec::new();
    let mut state = ScanState::Normal;
    let mut index = 0;

    while let Some(&(_, current)) = chars.get(index) {
        let next = char_at(index + 1);

        match state {
            ScanState::Normal => {
                let previous_is_identifier = index
                    .checked_sub(1)
                    .and_then(char_at)
                    .is_some_and(is_identifier_char);

                if current.is_whitespace() {
                    index += 1;
                    continue;
                }

                if current == '-' && next == Some('-') {
                    let after = char_at(index + 2);
                    let starts_comment = !rules.dash_comment_requires_space
                        || after.is_none_or(|after| after.is_whitespace() || after.is_control());

                    if starts_comment {
                        state = ScanState::LineComment;
                        index += 2;
                        continue;
                    }
                }

                if current == '#' && rules.hash_line_comments {
                    state = ScanState::LineComment;
                    index += 1;
                    continue;
                }

                if current == '/' && next == Some('*') {
                    // MySQL executes the body of `/*! ... */` and MariaDB also that of
                    // `/*M! ... */`, so it is scanned as code.
                    let executable_prefix_len = match (char_at(index + 2), char_at(index + 3)) {
                        (Some('!'), _) => Some(3),
                        (Some('M' | 'm'), Some('!')) => Some(4),
                        _ => None,
                    };

                    if rules.executable_comments
                        && let Some(prefix_len) = executable_prefix_len
                    {
                        index += prefix_len;

                        while char_at(index).is_some_and(|character| character.is_ascii_digit()) {
                            index += 1;
                        }

                        continue;
                    }

                    state = ScanState::BlockComment { depth: 1 };
                    index += 2;
                    continue;
                }

                let close = match current {
                    '\'' | '"' | '`' => Some(current),
                    '[' if rules.bracket_identifiers => Some(']'),
                    _ => None,
                };

                if let Some(close) = close {
                    let escape_prefixed = current == '\''
                        && rules.escape_string_prefix
                        && index
                            .checked_sub(1)
                            .and_then(char_at)
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&'e'))
                        && !index
                            .checked_sub(2)
                            .and_then(char_at)
                            .is_some_and(is_identifier_char);

                    let backslash_escapes = matches!(current, '\'' | '"')
                        && (rules.backslash_escapes || escape_prefixed);

                    tokens.push(Token::new(TokenKind::Quoted));
                    state = ScanState::Quoted {
                        close,
                        backslash_escapes,
                        content_start: index + 1,
                    };
                    index += 1;
                    continue;
                }

                if current == '$'
                    && rules.dollar_quotes
                    && !previous_is_identifier
                    && let Some(tag_len) = dollar_tag_len(&chars, index)
                {
                    tokens.push(Token::new(TokenKind::Quoted));
                    state = ScanState::DollarQuoted {
                        tag_start: index,
                        tag_len,
                    };
                    index += tag_len;
                    continue;
                }

                let starts_word = current.is_alphanumeric()
                    || current == '_'
                    || (current == '$' && !rules.dollar_quotes);

                if starts_word {
                    let mut word = String::new();

                    while let Some(character) = char_at(index).filter(|&c| is_identifier_char(c)) {
                        word.push(character.to_ascii_uppercase());
                        index += 1;
                    }

                    tokens.push(Token::new(TokenKind::Word(word)));
                    continue;
                }

                tokens.push(Token::new(TokenKind::Symbol(current)));
                index += 1;
            }

            ScanState::LineComment => {
                if current == '\n' {
                    state = ScanState::Normal;
                }

                index += 1;
            }

            ScanState::BlockComment { depth } => {
                if current == '*' && next == Some('/') {
                    state = if depth == 1 {
                        ScanState::Normal
                    } else {
                        ScanState::BlockComment { depth: depth - 1 }
                    };
                    index += 2;
                } else if rules.nested_block_comments && current == '/' && next == Some('*') {
                    state = ScanState::BlockComment { depth: depth + 1 };
                    index += 2;
                } else {
                    index += 1;
                }
            }

            ScanState::Quoted {
                close,
                backslash_escapes,
                content_start,
            } => {
                if backslash_escapes && current == '\\' {
                    index += 2;
                    continue;
                }

                if current == close {
                    if next == Some(close) {
                        index += 2;
                        continue;
                    }

                    // A `DO '...'` runs a single-quoted body as code, so keep
                    // what the literal holds (#507). An identifier in double
                    // quotes or brackets names an object, never code.
                    if close == '\'' {
                        let body =
                            quoted_body(&chars, content_start, index, close, backslash_escapes);

                        if let Some(token) = tokens.last_mut() {
                            token.quoted_body = Some(body);
                        }
                    }

                    state = ScanState::Normal;
                }

                index += 1;
            }

            ScanState::DollarQuoted { tag_start, tag_len } => {
                let closes =
                    (0..tag_len).all(|step| char_at(index + step) == char_at(tag_start + step));

                if closes {
                    // Keep the contents so a `DO` can read them as code: they
                    // are opaque to the rest of the lexer, which is why a row
                    // change inside the body was invisible (#497).
                    let body: String = chars
                        .get(tag_start + tag_len..index)
                        .unwrap_or_default()
                        .iter()
                        .map(|&(_, character)| character)
                        .collect();

                    if let Some(token) = tokens.last_mut() {
                        token.dollar_body = Some(body);
                    }

                    state = ScanState::Normal;
                    index += tag_len;
                } else {
                    index += 1;
                }
            }
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use datatab_policy::ExecutionClassification;

    use crate::QueryLanguage;

    use super::{
        BODY_STATEMENT_LEADERS, DIALECT_RULES, MAX_BODY_DEPTH, classify_query_for_governance,
        classify_sql_execution, classify_with_rules, contains_compound_block, is_safe_read_query,
    };

    #[test]
    fn allows_basic_read_queries() {
        assert!(is_safe_read_query("SELECT * FROM users"));
        assert!(is_safe_read_query(
            "with cte as (select 1) select * from cte"
        ));
        assert!(is_safe_read_query("SHOW TABLES"));
        assert!(is_safe_read_query("DESC users"));
    }

    #[test]
    fn rejects_write_queries() {
        assert!(!is_safe_read_query("INSERT INTO users VALUES (1)"));
        assert!(!is_safe_read_query("UPDATE users SET name = 'a'"));
        assert!(!is_safe_read_query("DELETE FROM users"));
        assert!(!is_safe_read_query("DROP TABLE users"));
    }

    #[test]
    fn rejects_multiple_statements() {
        assert!(!is_safe_read_query("SELECT 1; DROP TABLE users"));
        assert!(!is_safe_read_query("SELECT 1; SELECT 2"));
    }

    #[test]
    fn allows_single_statement_with_trailing_semicolon() {
        assert!(is_safe_read_query("SELECT 1;"));
        assert!(is_safe_read_query("-- comment\nSELECT 1;"));
    }

    #[test]
    fn strips_comments_before_keyword_detection() {
        assert!(is_safe_read_query("-- hello\nSELECT * FROM users"));
        assert!(is_safe_read_query("/* hello */ SELECT * FROM users"));
        assert!(!is_safe_read_query("/* hello */ DELETE FROM users"));
    }

    #[test]
    fn sql_classification_maps_read_write_and_destructive_classes() {
        assert_eq!(
            classify_sql_execution("SELECT * FROM users"),
            ExecutionClassification::Read
        );
        assert_eq!(
            classify_sql_execution("EXPLAIN SELECT * FROM users"),
            ExecutionClassification::Metadata
        );
        assert_eq!(
            classify_sql_execution("UPDATE users SET active = true WHERE id = 1"),
            ExecutionClassification::Write
        );
        assert_eq!(
            classify_sql_execution("DROP TABLE users"),
            ExecutionClassification::Destructive
        );
    }

    #[test]
    fn ambiguous_query_escalates_conservatively() {
        assert_eq!(
            classify_query_for_governance(&QueryLanguage::Sql, "VACUUM users", None),
            ExecutionClassification::Write
        );
    }

    #[test]
    fn language_service_delegation_overrides_the_conservative_default() {
        use super::super::language_service::{
            DangerousQueryKind, LanguageService, ValidationResult,
        };

        struct FakeService;

        impl LanguageService for FakeService {
            fn validate(&self, _query: &str) -> ValidationResult {
                ValidationResult::Valid
            }

            fn detect_dangerous(&self, _query: &str) -> Option<DangerousQueryKind> {
                None
            }

            fn classify_execution(&self, _query: &str) -> Option<ExecutionClassification> {
                Some(ExecutionClassification::Destructive)
            }
        }

        let service = FakeService;

        assert_eq!(
            classify_query_for_governance(
                &QueryLanguage::InfluxQuery,
                "DROP DATABASE x",
                Some(&service)
            ),
            ExecutionClassification::Destructive
        );

        assert_eq!(
            classify_query_for_governance(&QueryLanguage::InfluxQuery, "DROP DATABASE x", None),
            ExecutionClassification::Write
        );
    }

    fn assert_classified(queries: &[&str], expected: ExecutionClassification) {
        for query in queries {
            assert_eq!(
                classify_sql_execution(query),
                expected,
                "unexpected classification for: {query}"
            );
            assert!(
                !is_safe_read_query(query),
                "must not be a safe read: {query}"
            );
        }
    }

    fn assert_safe_reads(queries: &[&str]) {
        for query in queries {
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Read,
                "unexpected classification for: {query}"
            );
            assert!(is_safe_read_query(query), "must be a safe read: {query}");
        }
    }

    #[test]
    fn data_modifying_statements_inside_a_cte_are_writes() {
        assert_classified(
            &[
                "WITH d AS (DELETE FROM t WHERE id = 1 RETURNING *) SELECT * FROM d",
                "with i as (insert into t (id) values (1) returning id) select id from i",
                // A `MERGE` action after `THEN` is restricted by the `ON`
                // clause, not by a `WHERE`, so it is not a bare row change.
                "WITH m AS (MERGE INTO t USING s ON t.id = s.id \
                 WHEN MATCHED THEN DELETE RETURNING *) SELECT * FROM m",
            ],
            ExecutionClassification::Write,
        );
    }

    /// #489: a data-modifying CTE that removes or rewrites every row is the
    /// same shape as the bare statement, so it must not fold down to a keyed
    /// edit (`Write`) and run under a policy that allows row edits.
    #[test]
    fn a_where_less_row_change_inside_a_read_body_is_destructive() {
        assert_classified(
            &[
                "WITH u AS (UPDATE t SET a = 1 RETURNING *) SELECT * FROM u",
                "WITH x AS (SELECT 1) , d AS (DELETE FROM t RETURNING 1) SELECT * FROM x",
                "WITH cte AS (SELECT 1) DELETE FROM t",
            ],
            ExecutionClassification::Destructive,
        );
    }

    #[test]
    fn explain_analyze_is_classified_by_the_explained_statement() {
        assert_classified(
            &[
                "EXPLAIN ANALYZE DELETE FROM t WHERE id = 1",
                "EXPLAIN (ANALYZE) INSERT INTO t VALUES (1)",
                "EXPLAIN ANALYZE EXPLAIN ANALYZE SELECT 1",
                "EXPLAIN ANALYZE (EXPLAIN ANALYZE (SELECT 1))",
                "EXPLAIN (ANALYZE) (EXPLAIN (ANALYZE) SELECT 1)",
            ],
            ExecutionClassification::Write,
        );

        assert_eq!(
            classify_sql_execution("EXPLAIN ANALYZE SELECT * FROM t"),
            ExecutionClassification::Read
        );
        assert!(is_safe_read_query("EXPLAIN ANALYZE SELECT * FROM t"));
    }

    /// `EXPLAIN ANALYZE` runs the statement it explains, so a `WHERE`-less
    /// row change under it runs with no restricting condition either (#489).
    #[test]
    fn explain_analyze_of_a_where_less_row_change_is_destructive() {
        assert_classified(
            &[
                "explain analyse update t set a = 1",
                "EXPLAIN (ANALYZE true, BUFFERS) DELETE FROM t",
                "EXPLAIN (FORMAT JSON, ANALYZE) DELETE FROM t",
                "EXPLAIN ANALYZE VERBOSE WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
                "EXPLAIN ANALYZE FORMAT=TREE UPDATE t SET a = 1",
                "DESCRIBE ANALYZE UPDATE t SET a = 1",
                // PostgreSQL accepts a quoted identifier as an option name.
                "EXPLAIN (\"analyze\") DELETE FROM t",
                "EXPLAIN (FORMAT JSON, \"analyze\" true) DELETE FROM t",
            ],
            ExecutionClassification::Destructive,
        );
    }

    #[test]
    fn deeply_nested_explain_analyze_is_refused_without_deep_recursion() {
        let query = "EXPLAIN ANALYZE (".repeat(50_000);

        assert_eq!(
            classify_sql_execution(&query),
            ExecutionClassification::Write
        );
    }

    #[test]
    fn explain_without_analyze_stays_metadata() {
        for query in [
            "EXPLAIN DELETE FROM t WHERE id = 1",
            "EXPLAIN (FORMAT JSON) SELECT * FROM t",
            "EXPLAIN SELECT analyze FROM t",
            "EXPLAIN QUERY PLAN SELECT * FROM t",
            "DESC users",
        ] {
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Metadata,
                "unexpected classification for: {query}"
            );
            assert!(is_safe_read_query(query), "must be a safe read: {query}");
        }
    }

    #[test]
    fn select_into_is_a_write() {
        assert_classified(
            &[
                "SELECT * INTO new_table FROM t",
                "SELECT * INTO #tmp FROM t",
                "SELECT a, b INTO OUTFILE '/tmp/out.csv' FROM t",
                "SELECT a FROM t INTO DUMPFILE '/tmp/out.bin'",
                // `1.` is a numeric literal, not a qualifier.
                "SELECT 1. INTO t2",
            ],
            ExecutionClassification::Write,
        );
    }

    #[test]
    fn locking_reads_are_writes() {
        assert_classified(
            &[
                "SELECT * FROM t WHERE id = 1 FOR UPDATE",
                "SELECT * FROM t FOR UPDATE SKIP LOCKED",
                "SELECT * FROM t FOR SHARE",
                "SELECT * FROM t FOR NO KEY UPDATE",
                "SELECT * FROM t FOR KEY SHARE NOWAIT",
                "SELECT * FROM t LOCK IN SHARE MODE",
                "SELECT * FROM t WHERE id = 1. FOR SHARE",
            ],
            ExecutionClassification::Write,
        );
    }

    #[test]
    fn every_statement_of_a_batch_is_classified() {
        assert_eq!(
            classify_sql_execution("SELECT 1; DROP TABLE users"),
            ExecutionClassification::Destructive
        );
        assert_eq!(
            classify_sql_execution("SHOW TABLES; DELETE FROM users"),
            ExecutionClassification::Destructive
        );
        assert_eq!(
            classify_sql_execution("EXPLAIN SELECT 1; DELETE FROM users"),
            ExecutionClassification::Destructive
        );
        assert_eq!(
            classify_sql_execution("SELECT 1; SELECT 2"),
            ExecutionClassification::Write
        );
    }

    /// #489: the class of a `DELETE` or `UPDATE` says whether anything
    /// restricts it, in every dialect the shared heuristic covers. `--`
    /// starts a comment in all four, so the `WHERE` after it is text, not a
    /// filter.
    #[test]
    fn a_row_change_without_a_restricting_where_is_destructive() {
        for rules in &DIALECT_RULES {
            for query in [
                "DELETE FROM orders",
                "delete from Orders",
                "UPDATE orders SET total = 0",
                "DELETE FROM orders -- WHERE tenant = 'acme'",
                "UPDATE orders SET total = 0 /* WHERE id = 1 */",
            ] {
                assert_eq!(
                    classify_with_rules(query, rules),
                    ExecutionClassification::Destructive,
                    "unexpected classification for: {query}"
                );
            }
        }

        assert_classified(
            &[
                "DELETE FROM orders",
                "UPDATE orders SET total = 0",
                // MySQL reads `#` as a line comment, so its `WHERE` is text
                // there while the other three dialects read it as code; the
                // fold keeps the stricter reading.
                "DELETE FROM orders # WHERE tenant = 'acme'",
            ],
            ExecutionClassification::Destructive,
        );
    }

    /// The row-edit flow depends on a keyed `DELETE` and a `WHERE`-ed
    /// `UPDATE` staying `Write`. A guard, not a reproduction: it passed before
    /// #489 added the whole-table rule and must keep passing.
    #[test]
    fn a_keyed_row_change_stays_a_write() {
        assert_classified(
            &[
                "DELETE FROM orders WHERE id = 1",
                "UPDATE orders SET total = 0 WHERE id = 2",
                "DELETE FROM orders WHERE id IN (SELECT id FROM old_orders)",
                "UPDATE orders SET total = (SELECT sum(total) FROM lines) WHERE id = 3",
            ],
            ExecutionClassification::Write,
        );
    }

    /// A `WHERE` that only filters a subquery does not restrict the rows the
    /// statement touches. The dangerous-kind scan reads lowercased text and
    /// sees that `WHERE`, so it stays silent; the class must not.
    #[test]
    fn a_where_that_only_filters_a_subquery_does_not_restrict_the_row_change() {
        for query in [
            "UPDATE orders SET total = (SELECT max(total) FROM old_orders WHERE id = 1)",
            "DELETE FROM orders USING (SELECT id FROM old_orders WHERE id = 1) old",
        ] {
            assert_eq!(
                crate::query::language_service::detect_dangerous_query(query),
                None,
                "the text scan cannot place this WHERE: {query}"
            );
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "unexpected classification for: {query}"
            );
        }
    }

    /// #497: a row change inside a procedural body is still a row change. The
    /// splitter cuts a body at its inner `;`, so reading the leading word
    /// alone left the class at `Write` — a policy that allows row edits ran a
    /// whole-table removal with no dialog and no approval.
    ///
    /// A routine definition composes its body with the admin class its
    /// `CREATE` has, so the MySQL shape is `AdminDestructive`: above
    /// `Destructive` on both ladders, which is what the body asks for.
    #[test]
    fn a_where_less_row_change_inside_a_procedural_body_is_destructive() {
        for (query, expected) in [
            // PostgreSQL `BEGIN ... END` outside a transaction block.
            (
                "BEGIN DELETE FROM orders; END",
                ExecutionClassification::Destructive,
            ),
            (
                "BEGIN\n  UPDATE orders SET total = 0;\nEND",
                ExecutionClassification::Destructive,
            ),
            // PostgreSQL `DO $$ ... $$` and `DO $tag$ ... $tag$`.
            (
                "DO $$ BEGIN DELETE FROM orders; END $$",
                ExecutionClassification::Destructive,
            ),
            (
                "DO $body$ BEGIN UPDATE orders SET total = 0; END $body$",
                ExecutionClassification::Destructive,
            ),
            (
                "DO $$ DECLARE n int; BEGIN DELETE FROM orders; END $$",
                ExecutionClassification::Destructive,
            ),
            // MySQL `BEGIN ... END` inside a routine definition.
            (
                "CREATE PROCEDURE purge()\nBEGIN\n  DELETE FROM orders;\nEND",
                ExecutionClassification::AdminDestructive,
            ),
            (
                "CREATE TRIGGER orders_audit AFTER DELETE ON orders FOR EACH ROW\nBEGIN\n  UPDATE orders SET total = 0;\nEND",
                ExecutionClassification::AdminDestructive,
            ),
            // SQL Server `BEGIN ... END`, also as the body of an `IF`.
            (
                "BEGIN UPDATE orders SET total = 0; END",
                ExecutionClassification::Destructive,
            ),
            (
                "IF @x = 1 BEGIN DELETE FROM orders; END",
                ExecutionClassification::Destructive,
            ),
        ] {
            assert_eq!(
                classify_sql_execution(query),
                expected,
                "unexpected classification for: {query}"
            );
        }
    }

    /// The row-edit flow needs a keyed row change inside a body to stay an
    /// ordinary edit: only the unrestricted shapes are `Destructive`.
    #[test]
    fn a_keyed_row_change_inside_a_procedural_body_stays_a_write() {
        for (query, expected) in [
            (
                "BEGIN DELETE FROM orders WHERE id = 1; END",
                ExecutionClassification::Write,
            ),
            (
                "DO $$ BEGIN DELETE FROM orders WHERE id = 1; END $$",
                ExecutionClassification::Write,
            ),
            (
                "DO $body$ BEGIN UPDATE orders SET total = 0 WHERE id = 1; END $body$",
                ExecutionClassification::Write,
            ),
            // The definition still keeps the admin class its `CREATE` has.
            (
                "CREATE PROCEDURE purge() BEGIN DELETE FROM orders WHERE id < 10; END",
                ExecutionClassification::Admin,
            ),
            // A `MERGE`'s `THEN DELETE` is an action the `ON` clause guards,
            // not a statement a body opens, inside a body as outside one.
            (
                "BEGIN MERGE INTO orders USING orders_old ON orders.id = orders_old.id \
                 WHEN MATCHED THEN DELETE; END",
                ExecutionClassification::Write,
            ),
        ] {
            assert_eq!(
                classify_sql_execution(query),
                expected,
                "unexpected classification for: {query}"
            );
        }
    }

    /// A `BEGIN` that opens a transaction puts no statement after it, so the
    /// row changes of a transaction block are read where they start — at their
    /// own `;` — exactly as before #497.
    #[test]
    fn a_transaction_block_is_unchanged() {
        assert_eq!(
            classify_sql_execution("BEGIN; DELETE FROM orders WHERE id = 1; COMMIT;"),
            ExecutionClassification::Write
        );
        assert_eq!(
            classify_sql_execution(
                "BEGIN TRANSACTION; UPDATE orders SET total = 0 WHERE id = 1; COMMIT;"
            ),
            ExecutionClassification::Write
        );
        // #489 already reads the statement itself, whatever opened the block.
        assert_eq!(
            classify_sql_execution("BEGIN; DELETE FROM orders; COMMIT;"),
            ExecutionClassification::Destructive
        );
    }

    /// Only code is a body. A body-shaped string literal is a value, and a
    /// commented-out body is not run, so both stay a read.
    #[test]
    fn a_body_inside_a_string_or_a_comment_is_not_a_body() {
        assert_safe_reads(&[
            "SELECT 'BEGIN DELETE FROM orders; END' AS body",
            "SELECT 'DO $$ BEGIN DELETE FROM orders; END $$' AS body",
            "SELECT 1 -- BEGIN DELETE FROM orders; END\n",
            "SELECT 1 -- DO $$ BEGIN DELETE FROM orders; END $$\n",
            "SELECT 1 /* BEGIN UPDATE orders SET total = 0; END */",
            "SELECT 1 /* DO $$ BEGIN DELETE FROM orders; END $$ */",
        ]);

        // MySQL reads `$` as an identifier character, so it sees the words
        // inside a dollar-quoted string as code; the fold keeps that reading,
        // which is why this stays above `Read`. No body is read here: the class
        // was already `Write` before #497 and stays there.
        assert_eq!(
            classify_sql_execution("SELECT $$BEGIN DELETE FROM orders; END$$ AS body"),
            ExecutionClassification::Write
        );

        // Neither is an identifier named after a body keyword: `begin` as a
        // column is not a block, so nothing is read as its body.
        assert_safe_reads(&["SELECT begin FROM orders", "SELECT t.begin FROM orders"]);
        assert_eq!(
            classify_sql_execution("UPDATE orders SET begin = 1 WHERE id = 1"),
            ExecutionClassification::Write
        );
    }

    /// #483 holds for a body too: a buffer that also removes every row keeps
    /// the worst class when it folds.
    #[test]
    fn a_buffer_keeps_the_worst_class_of_a_procedural_body() {
        assert_eq!(
            classify_sql_execution("SELECT 1; DO $$ BEGIN DELETE FROM orders; END $$"),
            ExecutionClassification::Destructive
        );
        assert_eq!(
            classify_sql_execution(
                "CREATE TABLE orders_old (id int); BEGIN UPDATE orders SET total = 0; END"
            ),
            ExecutionClassification::AdminDestructive
        );
        assert_eq!(
            classify_sql_execution("SELECT 1; DO $$ BEGIN DELETE FROM orders WHERE id = 1; END $$"),
            ExecutionClassification::Write
        );
    }

    /// Deep nesting of a `DO` body stays bounded, and the level where the cap
    /// stops opening bodies is still read for the statements it holds: the
    /// innermost `BEGIN ... END` here sits at `MAX_BODY_DEPTH`.
    #[test]
    fn a_body_at_the_nesting_cap_is_still_read() {
        let mut query = String::from("BEGIN DELETE FROM orders; END");

        for level in 0..MAX_BODY_DEPTH {
            query = format!("DO $level{level}$ {query} $level{level}$");
        }

        assert_eq!(
            classify_sql_execution(&query),
            ExecutionClassification::Destructive,
            "a body {MAX_BODY_DEPTH} levels deep is still a body"
        );
    }

    /// The ruling on #497: the body classifier runs on every chunk, so a
    /// function named like a statement must not turn a plain read into a write.
    /// `THEN truncate(score, 2)` in a `CASE` arm names a function, and the
    /// guard is the one `classify_read_body` already applies to a write
    /// keyword.
    #[test]
    fn a_plain_read_stays_a_read() {
        assert_safe_reads(&[
            "SELECT CASE WHEN a > 0 THEN truncate(score, 2) ELSE score END FROM t",
            "SELECT CASE WHEN a > 0 THEN truncate(score, 2) ELSE score END FROM t WHERE a > 0",
            "SELECT CASE WHEN a > 0 THEN replace(name, 'a', 'b') ELSE name END FROM t",
            "SELECT lo, CASE WHEN lo > 0 THEN truncate(score, 2) END FROM t",
        ]);

        // The same guard keeps a keyed row edit a plain write.
        assert_eq!(
            classify_sql_execution(
                "UPDATE t SET score = CASE WHEN a > 0 THEN truncate(score, 2) ELSE score END \
                 WHERE id = 1"
            ),
            ExecutionClassification::Write
        );

        // A call is not a statement, but a statement that takes no call syntax
        // still is one.
        assert_eq!(
            classify_sql_execution("BEGIN truncate table orders; END"),
            ExecutionClassification::Destructive
        );
    }

    /// Every leader in the list opens a statement, not two of them: a row
    /// change after one is read even though the splitter cut no `;` in front of
    /// it. The list is read from the constant, so a leader added later is
    /// covered without touching this test.
    #[test]
    fn every_body_leader_is_read() {
        for leader in BODY_STATEMENT_LEADERS {
            let query = format!("{leader} DELETE FROM orders;");

            assert_eq!(
                classify_sql_execution(&query),
                ExecutionClassification::Destructive,
                "a row change after {leader} must be read"
            );
        }

        for query in [
            "WHILE 1 = 1 DELETE FROM orders;",
            "WHILE 1 = 1 BEGIN DELETE FROM orders; END",
            "DO DELETE FROM orders;",
            "DO $$ BEGIN WHILE 1 = 1 LOOP DELETE FROM orders; END LOOP; END $$",
            "IF @x = 1 THEN DELETE FROM orders; END IF",
            "IF @x = 0 THEN SELECT 1; ELSE DELETE FROM orders; END IF",
            "LOOP DELETE FROM orders; END LOOP",
        ] {
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "unexpected classification for: {query}"
            );
        }
    }

    /// One table for the whole family: a clause keyword is a keyword only where
    /// its clause puts it, so a name spelled the same way leaves the body
    /// statement in the arm read. Each row names its shape and says whether the
    /// class comes from reading a body statement or from leaving an action to
    /// the statement that owns it.
    #[test]
    fn a_clause_keyword_counts_only_in_its_own_clause() {
        use ExecutionClassification::{Destructive, Write};

        // `body` means the row change in the arm is read on its own; `action`
        // means the arm belongs to the statement that spells the clause, so the
        // class stays that statement's.
        for (query, expected, kind) in [
            (
                "MERGE INTO orders USING orders_old ON orders.id = orders_old.id \
                 WHEN MATCHED THEN DELETE",
                Write,
                "action",
            ),
            (
                "MERGE INTO orders USING orders_old ON orders.id = orders_old.id \
                 WHEN NOT MATCHED THEN INSERT (id) VALUES (orders_old.id)",
                Write,
                "action",
            ),
            (
                "MERGE INTO orders USING orders_old ON orders.id = orders_old.id \
                 WHEN NOT MATCHED BY TARGET THEN INSERT (id) VALUES (orders_old.id)",
                Write,
                "action",
            ),
            (
                "MERGE INTO orders USING orders_old ON orders.id = orders_old.id \
                 WHEN MATCHED AND orders.total = 0 THEN DELETE",
                Write,
                "action",
            ),
            (
                "INSERT INTO orders (id) VALUES (1) ON CONFLICT (id) DO UPDATE SET total = 0",
                Write,
                "action",
            ),
            (
                "INSERT INTO orders (id) VALUES (1) ON CONFLICT ON CONSTRAINT orders_pkey DO NOTHING",
                Write,
                "action",
            ),
            (
                "IF r.matched THEN DELETE FROM orders; END IF",
                Destructive,
                "body",
            ),
            (
                "IF NOT matched THEN DELETE FROM orders; END IF",
                Destructive,
                "body",
            ),
            (
                "IF (SELECT count(*) FROM merge) > 0 THEN DELETE FROM orders; END IF",
                Destructive,
                "body",
            ),
            ("WHILE conflict DO DELETE FROM orders;", Destructive, "body"),
        ] {
            assert_eq!(
                classify_sql_execution(query),
                expected,
                "a {kind} row for: {query}"
            );
        }
    }

    /// A `WHILE` is a body keyword only where a statement starts, so a column
    /// named `while` does not make the rest of a read a body.
    #[test]
    fn a_while_that_is_a_column_name_is_not_a_while_loop() {
        assert_safe_reads(&[
            "SELECT while, created_at FROM t",
            "SELECT * FROM t WHERE while > 0",
        ]);

        // The `delete` name is over-classified on its own by
        // `classify_read_body`, as it was before #497. What the body scan must
        // not do is push it to `Destructive`.
        for query in [
            "SELECT while, delete FROM t",
            "SELECT * FROM t WHERE while > 0 AND delete = 1",
        ] {
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Write,
                "unexpected classification for: {query}"
            );
        }
    }

    /// The ruling on #497: a `DO` body past the depth cap is not opened again,
    /// so its worst statement is unknown and the class stays conservative
    /// rather than falling back to a plain `Write`.
    #[test]
    fn a_body_past_the_nesting_cap_stays_destructive() {
        let mut query = String::from("BEGIN PERFORM 1; END");

        for level in 0..=MAX_BODY_DEPTH {
            query = format!("DO $level{level}$ {query} $level{level}$");
        }

        assert_eq!(
            classify_sql_execution(&query),
            ExecutionClassification::Destructive,
            "a body past the cap is read as body code, not as a plain write"
        );
    }

    /// A block that holds a definition composes the admin class of that
    /// statement: a `CREATE TABLE IF NOT EXISTS` inside a `BEGIN ... END` is
    /// `Admin`, above the `Write` of the block itself.
    #[test]
    fn a_definition_inside_a_body_keeps_its_admin_class() {
        assert_eq!(
            classify_sql_execution("BEGIN CREATE TABLE IF NOT EXISTS orders_old (id int); END"),
            ExecutionClassification::Admin
        );
        assert_eq!(
            classify_sql_execution("BEGIN GRANT SELECT ON orders TO app; END"),
            ExecutionClassification::Admin
        );
    }

    /// The point of #497: policy reads the class alone, so a `Write`-only
    /// policy must not run a body that removes or rewrites every row.
    #[test]
    fn a_write_only_policy_denies_a_row_change_inside_a_body() {
        use datatab_policy::{ClassDecision, ToolPolicy};

        let policy = ToolPolicy {
            id: "policy".to_string(),
            allowed_tools: vec!["execute_query".to_string()],
            allowed_classes: vec![
                ExecutionClassification::Metadata,
                ExecutionClassification::Read,
                ExecutionClassification::Write,
            ],
            approval_classes: Vec::new(),
        };

        for query in [
            "DO $$ BEGIN DELETE FROM orders; END $$",
            "BEGIN UPDATE orders SET total = 0; END",
            "CREATE PROCEDURE purge() BEGIN DELETE FROM orders; END",
        ] {
            let classification = classify_query_for_governance(&QueryLanguage::Sql, query, None);

            assert_eq!(
                policy.class_decision(classification),
                ClassDecision::Deny,
                "a policy that allows only row edits must not run: {query}"
            );
        }

        let keyed = classify_query_for_governance(
            &QueryLanguage::Sql,
            "DO $$ BEGIN DELETE FROM orders WHERE id = 1; END $$",
            None,
        );

        assert_eq!(policy.class_decision(keyed), ClassDecision::Allow);
    }

    /// #483: a buffer is never less restricted than any of its statements, on
    /// either ladder, so mixing a destructive statement (DROP, TRUNCATE, ALTER)
    /// with an admin one escalates to `AdminDestructive` rather than folding
    /// down to the admin class.
    #[test]
    fn a_buffer_that_mixes_a_destructive_statement_with_an_admin_one_is_admin_destructive() {
        assert_eq!(
            classify_sql_execution("DROP TABLE old_orders; CREATE TABLE new_orders (id int)"),
            ExecutionClassification::AdminDestructive
        );
        assert_eq!(
            classify_sql_execution("TRUNCATE TABLE events; CREATE INDEX events_at ON events (at)"),
            ExecutionClassification::AdminDestructive
        );
        assert_eq!(
            classify_sql_execution(
                "ALTER TABLE events ADD COLUMN at timestamp; GRANT SELECT ON events TO app"
            ),
            ExecutionClassification::AdminDestructive
        );
    }

    /// A buffer with no destructive statement keeps the class its worst
    /// statement has: creating a table and filling it never escalates.
    #[test]
    fn a_create_table_and_insert_buffer_stays_admin() {
        assert_eq!(
            classify_sql_execution("CREATE TABLE seed (id int); INSERT INTO seed VALUES (1)"),
            ExecutionClassification::Admin
        );
    }

    /// #489 with the #483 invariant: a buffer that also removes or rewrites
    /// every row in one statement keeps the worst class its statements have,
    /// so no policy reads it as a plain write.
    #[test]
    fn a_buffer_keeps_a_where_less_row_change_when_it_folds() {
        assert_eq!(
            classify_sql_execution("SELECT 1; DELETE FROM orders"),
            ExecutionClassification::Destructive
        );
        assert_eq!(
            classify_sql_execution("UPDATE orders SET total = 0; SELECT 1"),
            ExecutionClassification::Destructive
        );
        assert_eq!(
            classify_sql_execution("DELETE FROM orders; CREATE TABLE orders_old (id int)"),
            ExecutionClassification::AdminDestructive
        );
        // A keyed delete is still an ordinary row edit in a buffer.
        assert_eq!(
            classify_sql_execution("SELECT 1; DELETE FROM orders WHERE id = 1"),
            ExecutionClassification::Write
        );
    }

    /// A destructive statement is hidden from three of the four dialects by a
    /// MySQL executable comment, so only the per-dialect fold sees it. Left on
    /// `max`, the buffer folds down to `Admin` and a policy that allows admin
    /// changes runs the drop.
    #[test]
    fn a_destructive_statement_hidden_in_an_executable_comment_is_not_folded_away() {
        assert_eq!(
            classify_sql_execution("/*! DROP TABLE events */ CREATE TABLE events_new (id int)"),
            ExecutionClassification::AdminDestructive
        );
    }

    /// A read body that names a destructive and an admin keyword escalates, so
    /// the body can never hide the worse of the two it mentions. Keywords are
    /// read as code inside a read body on purpose (see the module doc), which
    /// over-classifies a column merely called `drop`.
    #[test]
    fn a_read_body_that_names_a_destructive_and_an_admin_keyword_is_admin_destructive() {
        assert_eq!(
            classify_sql_execution("SELECT drop, create FROM t"),
            ExecutionClassification::AdminDestructive
        );
    }

    /// The policy consequence of that fold: a policy that allows the admin
    /// classes and nothing worse refuses the mixed buffer.
    #[test]
    fn a_policy_that_allows_admin_classes_refuses_a_buffer_that_drops_and_creates() {
        use datatab_policy::{ClassDecision, ToolPolicy};

        let buffer = classify_sql_execution(
            "DROP TABLE old_orders; CREATE TABLE new_orders (id int PRIMARY KEY)",
        );

        for allowed_classes in [
            vec![
                ExecutionClassification::Metadata,
                ExecutionClassification::Read,
                ExecutionClassification::AdminSafe,
            ],
            vec![
                ExecutionClassification::Metadata,
                ExecutionClassification::Read,
                ExecutionClassification::AdminSafe,
                ExecutionClassification::Admin,
            ],
        ] {
            let policy = ToolPolicy {
                id: "policy".to_string(),
                allowed_tools: vec!["execute_script".to_string()],
                allowed_classes: allowed_classes.clone(),
                approval_classes: Vec::new(),
            };

            assert_eq!(
                policy.class_decision(buffer),
                ClassDecision::Deny,
                "a policy allowing only {allowed_classes:?} must not run a buffer that drops and creates"
            );
        }
    }

    /// Reviewer 2's shape on #483: a policy that allows `Destructive` and asks
    /// on the admin classes. The buffer used to be `Admin` (ask, a human saw
    /// it) and must not become anything the policy allows outright.
    #[test]
    fn a_policy_that_allows_destructive_and_asks_on_admin_never_allows_the_buffer() {
        use datatab_policy::{ClassDecision, ToolPolicy};

        let buffer = classify_sql_execution(
            "DROP TABLE old_orders; CREATE TABLE new_orders (id int PRIMARY KEY)",
        );

        let policy = ToolPolicy {
            id: "policy".to_string(),
            allowed_tools: vec!["execute_script".to_string()],
            allowed_classes: vec![
                ExecutionClassification::Metadata,
                ExecutionClassification::Read,
                ExecutionClassification::Write,
                ExecutionClassification::Destructive,
            ],
            approval_classes: vec![
                ExecutionClassification::AdminSafe,
                ExecutionClassification::Admin,
            ],
        };

        assert_ne!(
            policy.class_decision(buffer),
            ClassDecision::Allow,
            "the buffer must never run without a human under this policy"
        );
        assert_eq!(policy.class_decision(buffer), ClassDecision::Deny);
    }

    /// #489: the MCP query path rates the text, then asks the policy about the
    /// class alone (`datatab_mcp::handlers::query`). A policy that allows
    /// `Write` — the row-edit grant — must not run a whole-table `DELETE` or
    /// `UPDATE` with no dialog and no approval.
    #[test]
    fn a_write_only_policy_denies_a_where_less_row_change() {
        use datatab_policy::{ClassDecision, ToolPolicy};

        let policy = ToolPolicy {
            id: "policy".to_string(),
            allowed_tools: vec!["execute_query".to_string()],
            allowed_classes: vec![
                ExecutionClassification::Metadata,
                ExecutionClassification::Read,
                ExecutionClassification::Write,
            ],
            approval_classes: Vec::new(),
        };

        for query in ["DELETE FROM orders", "UPDATE orders SET total = 0"] {
            let classification = classify_query_for_governance(&QueryLanguage::Sql, query, None);

            assert_eq!(
                policy.class_decision(classification),
                ClassDecision::Deny,
                "a policy that allows only row edits must not run: {query}"
            );
        }

        let keyed = classify_query_for_governance(
            &QueryLanguage::Sql,
            "DELETE FROM orders WHERE id = 1",
            None,
        );

        assert_eq!(policy.class_decision(keyed), ClassDecision::Allow);
    }

    /// The UI hard-confirms a `WHERE`-less `DELETE`/`UPDATE` from the
    /// dangerous-kind scan, and the class now says the same thing, so the
    /// dialog and the policy decision agree on both shapes.
    #[test]
    fn the_confirm_dialog_and_the_class_agree_for_both_shapes() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (query, expected_kind, expected_class) in [
            (
                "DELETE FROM orders",
                Some(DangerousQueryKind::DeleteNoWhere),
                ExecutionClassification::Destructive,
            ),
            (
                "UPDATE orders SET total = 0",
                Some(DangerousQueryKind::UpdateNoWhere),
                ExecutionClassification::Destructive,
            ),
            (
                "DELETE FROM orders WHERE id = 1",
                None,
                ExecutionClassification::Write,
            ),
            (
                "UPDATE orders SET total = 0 WHERE id = 1",
                None,
                ExecutionClassification::Write,
            ),
        ] {
            assert_eq!(detect_dangerous_query(query), expected_kind);
            assert_eq!(classify_sql_execution(query), expected_class);
        }
    }

    /// #507: the dangerous-kind scan reads the same statements the class
    /// reads, so a body that removes or rewrites every row names the pattern in
    /// the hard-confirm dialog. Before this the class refused the body under a
    /// `write`-only policy (#497) while the dialog saw nothing: it split the
    /// buffer at the body's inner `;` and never opened the body.
    #[test]
    fn the_confirm_dialog_reads_the_bodies_the_class_reads() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (shape, query, expected_kind) in [
            (
                "PostgreSQL BEGIN ... END",
                "BEGIN DELETE FROM orders; END",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "PostgreSQL DO with a dollar body",
                "DO $$ BEGIN DELETE FROM orders; END $$",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "PostgreSQL DO with a tagged body",
                "DO $body$ BEGIN UPDATE orders SET total = 0; END $body$",
                DangerousQueryKind::UpdateNoWhere,
            ),
            (
                "PostgreSQL DO with a single-quoted body",
                "DO 'BEGIN DELETE FROM orders; END'",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "PostgreSQL EXECUTE with a dollar body",
                "EXECUTE $q$DELETE FROM orders$q$",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "T-SQL IF with no BEGIN",
                "IF @x = 1 DELETE FROM orders",
                DangerousQueryKind::DeleteNoWhere,
            ),
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                Some(expected_kind),
                "{shape}: the dialog must name the worst statement of the body"
            );
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "{shape}: the class and the dialog must agree"
            );
        }
    }

    /// The union across dialects holds for the dialog too: a `WHERE` that only
    /// one dialect reads as a comment is text to the class, so the dialog must
    /// not treat it as a filter. Before #507 the dialog read the raw text and
    /// stayed silent, while the class refused the statement.
    #[test]
    fn a_commented_out_where_cannot_hide_a_row_change_from_the_dialog() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (query, expected_kind) in [
            (
                "DELETE FROM orders -- WHERE tenant = 'acme'",
                DangerousQueryKind::DeleteNoWhere,
            ),
            // MySQL reads `#` as a line comment; the other three dialects read
            // the `WHERE` as code, so the union keeps the worse reading on both
            // ladders.
            (
                "DELETE FROM orders # WHERE tenant = 'acme'",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "UPDATE orders SET total = 0 /* WHERE id = 1 */",
                DangerousQueryKind::UpdateNoWhere,
            ),
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                Some(expected_kind),
                "{query}"
            );
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "{query}"
            );
        }
    }

    /// The same shapes with a keyed row change inside: the row-edit flow keeps
    /// working, so the class stays `Write` and no kind fires.
    #[test]
    fn a_keyed_row_change_inside_a_body_shows_no_kind() {
        use super::super::language_service::detect_dangerous_query;

        for (shape, query) in [
            (
                "PostgreSQL BEGIN ... END",
                "BEGIN DELETE FROM orders WHERE id = 1; END",
            ),
            (
                "PostgreSQL DO with a dollar body",
                "DO $$ BEGIN DELETE FROM orders WHERE id = 1; END $$",
            ),
            (
                "PostgreSQL DO with a single-quoted body",
                "DO 'BEGIN DELETE FROM orders WHERE id = 1; END'",
            ),
            (
                "PostgreSQL EXECUTE with a dollar body",
                "EXECUTE $q$DELETE FROM orders WHERE id = 1$q$",
            ),
            (
                "T-SQL IF with no BEGIN",
                "IF @x = 1 DELETE FROM orders WHERE id = 1",
            ),
        ] {
            assert_eq!(detect_dangerous_query(query), None, "{shape}");
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Write,
                "{shape}: a keyed row change inside a body is an ordinary edit"
            );
        }
    }

    /// #513: a data-modifying CTE is a statement of its own inside a read
    /// body. The class reads it through `starts_nested_statement`, but the
    /// dialog walked the chunk to its trailing `SELECT` and named nothing, so
    /// the policy refused the statement while the dialog stayed silent.
    #[test]
    fn a_data_modifying_cte_names_the_pattern_of_its_body() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (shape, query, expected_kind) in [
            (
                "a CTE that removes every row",
                "WITH d AS (DELETE FROM t) SELECT 1",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "a CTE that rewrites every row",
                "WITH u AS (UPDATE t SET a = 1) SELECT 1",
                DangerousQueryKind::UpdateNoWhere,
            ),
            (
                "a reading CTE beside a writing one",
                "WITH r AS (SELECT 1), w AS (DELETE FROM t) SELECT * FROM r",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "a nested WITH inside the body",
                "WITH a AS (WITH b AS (DELETE FROM t) SELECT * FROM b) SELECT * FROM a",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "a CTE inside a transaction block",
                "BEGIN WITH d AS (DELETE FROM t) SELECT 1; END",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "a WHERE on the trailing statement is not the body's",
                "WITH d AS (DELETE FROM t) SELECT * FROM d WHERE x = 1",
                DangerousQueryKind::DeleteNoWhere,
            ),
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                Some(expected_kind),
                "{shape}: the dialog must name the body's pattern"
            );
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "{shape}: the class and the dialog must agree"
            );
        }
    }

    /// A plan runs none of the statement it wraps, so the body a read body now
    /// names must stay unnamed under it. Before this guard the class said
    /// `Metadata` while the dialog hard-confirmed a removal that never runs
    /// (#513).
    #[test]
    fn an_explain_that_does_not_run_names_no_body() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (shape, query) in [
            ("EXPLAIN", "EXPLAIN WITH d AS (DELETE FROM t) SELECT 1"),
            (
                "EXPLAIN with an option list",
                "EXPLAIN (FORMAT JSON) WITH d AS (DELETE FROM t) SELECT 1",
            ),
        ] {
            assert_eq!(detect_dangerous_query(query), None, "{shape}");
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Metadata,
                "{shape}: a plan runs nothing"
            );
        }

        let analyze = "EXPLAIN ANALYZE WITH d AS (DELETE FROM t) SELECT 1";
        assert_eq!(
            detect_dangerous_query(analyze),
            Some(DangerousQueryKind::DeleteNoWhere)
        );
        assert_eq!(
            classify_sql_execution(analyze),
            ExecutionClassification::Destructive,
            "ANALYZE runs the statement it explains"
        );

        // The shapes #489 already pinned keep their reading.
        let plain = "WITH d AS (DELETE FROM t) SELECT 1";
        assert_eq!(
            detect_dangerous_query(plain),
            Some(DangerousQueryKind::DeleteNoWhere)
        );
        assert_eq!(
            classify_sql_execution(plain),
            ExecutionClassification::Destructive
        );
        assert_eq!(detect_dangerous_query("EXPLAIN DELETE FROM t"), None);
        assert_eq!(
            classify_sql_execution("EXPLAIN DELETE FROM t"),
            ExecutionClassification::Metadata
        );
        // #515 reads the target an executing explain wraps, so this shape now
        // names the removal it runs; it read `None` before that fix.
        assert_eq!(
            detect_dangerous_query("EXPLAIN ANALYZE DELETE FROM t"),
            Some(DangerousQueryKind::DeleteNoWhere)
        );
        assert_eq!(
            classify_sql_execution("EXPLAIN ANALYZE DELETE FROM t"),
            ExecutionClassification::Destructive
        );
    }

    /// #515: `EXPLAIN ANALYZE` runs the statement it wraps, so the class
    /// refuses it, but the text rules read the chunk as `EXPLAIN ...` and named
    /// no pattern: a policy refused while the dialog said nothing. The target
    /// the class already reads now names the pattern too.
    #[test]
    fn an_explain_analyze_names_the_pattern_it_runs() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (shape, query, expected_kind) in [
            (
                "a whole-table removal",
                "EXPLAIN ANALYZE DELETE FROM t",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "a whole-table rewrite",
                "EXPLAIN ANALYZE UPDATE t SET a = 1",
                DangerousQueryKind::UpdateNoWhere,
            ),
            (
                "an ANALYZE inside an option list",
                "EXPLAIN (ANALYZE, FORMAT JSON) DELETE FROM t",
                DangerousQueryKind::DeleteNoWhere,
            ),
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                Some(expected_kind),
                "{shape}: the dialog must name the statement the explain runs"
            );
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "{shape}: the class and the dialog must agree"
            );
        }

        // A plan runs nothing, so it names nothing.
        let plan = "EXPLAIN DELETE FROM t";
        assert_eq!(detect_dangerous_query(plan), None, "{plan}");
        assert_eq!(
            classify_sql_execution(plan),
            ExecutionClassification::Metadata,
            "{plan}"
        );

        for (shape, query) in [
            (
                "a keyed removal",
                "EXPLAIN ANALYZE DELETE FROM t WHERE id = 1",
            ),
            (
                "a keyed rewrite",
                "EXPLAIN ANALYZE UPDATE t SET a = 1 WHERE id = 1",
            ),
        ] {
            assert_eq!(detect_dangerous_query(query), None, "{shape}");
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Write,
                "{shape}: a keyed row change stays an ordinary edit"
            );
        }

        // The kind #513 gave a data-modifying CTE stays, and the `EXPLAIN
        // ANALYZE` inside a `DO` body runs a string that is read the same way.
        for (shape, query) in [
            (
                "a data-modifying CTE the explain runs",
                "EXPLAIN ANALYZE WITH d AS (DELETE FROM t) SELECT 1",
            ),
            (
                "an EXPLAIN ANALYZE inside a DO body",
                "DO $$ EXPLAIN ANALYZE DELETE FROM t $$",
            ),
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                Some(DangerousQueryKind::DeleteNoWhere),
                "{shape}"
            );
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "{shape}"
            );
        }

        // A `BEGIN ... END` body puts its statement after the block keyword,
        // not at `EXPLAIN`, so neither ladder starts a statement there and both
        // keep the reading they had.
        let block = "BEGIN EXPLAIN ANALYZE DELETE FROM t; END";
        assert_eq!(detect_dangerous_query(block), None, "{block}");
        assert_eq!(
            classify_sql_execution(block),
            ExecutionClassification::Write,
            "{block}"
        );
    }

    /// The same shapes with a keyed row change inside, and a CTE that only
    /// reads: the row-edit flow keeps working, so the class stays `Write` and
    /// no kind fires. A guard, not a reproduction: it passed before #513 and
    /// must keep passing.
    #[test]
    fn a_keyed_row_change_inside_a_cte_shows_no_kind() {
        use super::super::language_service::detect_dangerous_query;

        for query in [
            "WITH d AS (DELETE FROM t WHERE id = 1) SELECT 1",
            "WITH u AS (UPDATE t SET a = 1 WHERE id = 1) SELECT 1",
        ] {
            assert_eq!(detect_dangerous_query(query), None, "{query}");
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Write,
                "{query}: a keyed row change inside a CTE is an ordinary edit"
            );
        }

        let read_only = "WITH s AS (SELECT 1) SELECT * FROM s";
        assert_eq!(detect_dangerous_query(read_only), None, "{read_only}");
        assert_eq!(
            classify_sql_execution(read_only),
            ExecutionClassification::Read,
            "{read_only}"
        );
    }

    /// #483 holds for the dialog too: a buffer keeps the worst class and the
    /// kind of the worst statement it holds, body or not.
    #[test]
    fn a_buffer_with_a_body_keeps_the_kind_of_its_worst_statement() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (query, expected_kind, expected_class) in [
            (
                "SELECT 1; DO $$ BEGIN DELETE FROM orders; END $$",
                DangerousQueryKind::DeleteNoWhere,
                ExecutionClassification::Destructive,
            ),
            (
                "CREATE TABLE orders_old (id int); BEGIN UPDATE orders SET total = 0; END",
                DangerousQueryKind::UpdateNoWhere,
                ExecutionClassification::AdminDestructive,
            ),
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                Some(expected_kind),
                "unexpected kind for: {query}"
            );
            assert_eq!(
                classify_sql_execution(query),
                expected_class,
                "unexpected class for: {query}"
            );
        }
    }

    /// The other direction: a plain read and a keyed edit must not gain a kind.
    /// `truncate(score, 2)` is a function call, not a statement, and a keyed row
    /// change stays an ordinary edit.
    #[test]
    fn a_plain_read_and_a_keyed_edit_gain_no_kind() {
        use super::super::language_service::detect_dangerous_query;

        for query in [
            "SELECT CASE WHEN a > 0 THEN truncate(score, 2) ELSE score END FROM t",
            "SELECT REPLACE(name, 'a', 'b') FROM t",
            "DELETE FROM orders WHERE id = 1",
            "UPDATE orders SET total = 0 WHERE id = 1",
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                None,
                "gained a kind: {query}"
            );
        }

        assert_eq!(
            classify_sql_execution(
                "SELECT CASE WHEN a > 0 THEN truncate(score, 2) ELSE score END FROM t"
            ),
            ExecutionClassification::Read
        );
    }

    /// #507 change 3: a `;` inside a quoted value no longer splits the buffer,
    /// because the class never split there. A value holding statement text used
    /// to raise `Script`, so the dialog hard-confirmed a plain insert; a real
    /// second statement is still seen.
    #[test]
    fn a_quoted_statement_text_is_not_a_second_statement() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        let insert_holding_a_delete = "INSERT INTO log (note) VALUES ('a; DELETE FROM t')";

        assert_eq!(
            detect_dangerous_query(insert_holding_a_delete),
            None,
            "a `;` inside a value is not a statement boundary"
        );
        assert_eq!(
            classify_sql_execution(insert_holding_a_delete),
            ExecutionClassification::Write
        );

        let real_second_statement = "INSERT INTO log (note) VALUES ('a; b'); DELETE FROM t";

        assert_eq!(
            detect_dangerous_query(real_second_statement),
            Some(DangerousQueryKind::Script),
            "a statement after the value is still its own statement"
        );
        assert_eq!(
            classify_sql_execution(real_second_statement),
            ExecutionClassification::Destructive
        );

        // A body spelled inside a literal is a value: only `DO` and `EXECUTE`
        // open one.
        let body_in_a_literal = "SELECT 'DO $$ BEGIN DELETE FROM orders; END $$' AS body";

        assert_eq!(detect_dangerous_query(body_in_a_literal), None);
        assert_eq!(
            classify_sql_execution(body_in_a_literal),
            ExecutionClassification::Read
        );
    }

    /// #507 change 3: a definition's `BEGIN ... END` body is read by the dialog
    /// too, matching the class #497 gave it. Nothing runs the body here; see
    /// `classify_executed_body` for why a quoted `AS $$ ... $$` body differs.
    #[test]
    fn a_definition_body_names_its_pattern_in_the_dialog() {
        use super::super::language_service::{DangerousQueryKind, detect_dangerous_query};

        for (query, expected_kind) in [
            (
                "CREATE PROCEDURE p() BEGIN DELETE FROM orders; END",
                DangerousQueryKind::DeleteNoWhere,
            ),
            (
                "CREATE TRIGGER orders_audit AFTER DELETE ON orders FOR EACH ROW \
                 BEGIN UPDATE orders SET total = 0; END",
                DangerousQueryKind::UpdateNoWhere,
            ),
        ] {
            assert_eq!(
                detect_dangerous_query(query),
                Some(expected_kind),
                "{query}"
            );
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::AdminDestructive,
                "{query}"
            );
        }
    }

    #[test]
    fn writes_hidden_by_dialect_specific_lexing_are_detected() {
        assert_classified(
            &[
                // MySQL executes the body of a `/*! ... */` comment.
                "SELECT a FROM t /*!50000 INTO OUTFILE '/tmp/x' */",
                // MariaDB also executes the body of `/*M! ... */`.
                "SELECT a FROM t /*M!100000 INTO OUTFILE '/tmp/x' */",
                "SELECT a FROM t /*M! INTO OUTFILE '/tmp/x' */",
                // MySQL reads `\'` as an escaped quote, so the string closes early.
                r"SELECT 'a\'' , b INTO OUTFILE '/tmp/x' FROM t",
                // PostgreSQL block comments nest.
                "SELECT 1 /* /* */ ' */ , 2 INTO t2 FROM t -- '",
                // MySQL needs whitespace after `--` to start a comment.
                "SELECT 1--1 INTO OUTFILE '/tmp/x'",
            ],
            ExecutionClassification::Write,
        );
    }

    #[test]
    fn compound_blocks_are_told_apart_from_transactions() {
        let procedure = "CREATE PROCEDURE purge_logs()\nBEGIN\n  DELETE FROM logs WHERE id < 10;\n  INSERT INTO audit (action) VALUES ('purge');\nEND;";
        assert!(contains_compound_block(procedure));
        assert!(contains_compound_block(
            "BEGIN TRY\n  SELECT 1;\nEND TRY\nBEGIN CATCH\n  SELECT 2;\nEND CATCH;"
        ));
        assert!(contains_compound_block("BEGIN NOT ATOMIC SELECT 1; END;"));

        for transaction in [
            "BEGIN; SELECT 1; COMMIT;",
            "BEGIN",
            "BEGIN TRANSACTION; SELECT 1; COMMIT;",
            "BEGIN TRAN; SELECT 1; COMMIT TRAN;",
            "BEGIN WORK; SELECT 1; COMMIT;",
            "BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT 1; COMMIT;",
            "BEGIN IMMEDIATE; SELECT 1; COMMIT;",
        ] {
            assert!(!contains_compound_block(transaction), "{transaction}");
        }

        assert!(!contains_compound_block(
            "DO $$ BEGIN PERFORM 1; END $$; SELECT 1;"
        ));
        assert!(!contains_compound_block("SELECT 'BEGIN x'; SELECT 1;"));
        assert!(contains_compound_block(
            "CREATE PROCEDURE p AS BEGIN SELECT 1; SELECT 2; END;"
        ));
        assert!(contains_compound_block(
            "IF @x = 1 BEGIN SELECT 1; SELECT 2; END"
        ));

        for identifier in [
            "SELECT begin FROM t; SELECT 2;",
            "SELECT t.begin, x FROM t; SELECT 2;",
            "UPDATE t SET begin = 1 WHERE begin > 0; SELECT 2;",
            "SELECT * FROM t ORDER BY begin DESC; SELECT 2;",
            "SELECT begin AS started FROM t; SELECT 2;",
        ] {
            assert!(!contains_compound_block(identifier), "{identifier}");
        }
        assert!(!contains_compound_block("-- BEGIN x\nSELECT 1; SELECT 2;"));
    }

    #[test]
    fn keywords_in_names_literals_and_comments_do_not_reclassify_reads() {
        assert_safe_reads(&[
            "SELECT updated_at, deleted, insert_count, created_by FROM t",
            "SELECT * FROM t WHERE status = 'DELETE'",
            "SELECT * FROM t WHERE note = 'drop it; insert into x'",
            "SELECT \"update\", \"delete\" FROM \"insert\"",
            "SELECT `update`, `into` FROM `delete`",
            "SELECT [update] FROM [dbo].[delete]",
            "SELECT t.update, t.into FROM t",
            "SELECT 1 -- DELETE FROM t",
            "SELECT /* DROP TABLE t */ 1",
            "SELECT $$DELETE FROM t$$ AS body",
            "SELECT $body$DELETE$body$ AS a, $$DROP$$ AS b",
            "SELECT REPLACE(name, 'a', 'b'), TRUNCATE(price, 2) FROM t",
            "SELECT * FROM t FOR SYSTEM_TIME AS OF '2020-01-01'",
            "SELECT * FROM t FOR JSON PATH",
            r"SELECT 'it''s' AS a, E'don\'t' AS b FROM t",
            "WITH recent AS (SELECT * FROM t WHERE updated_at > now()) SELECT * FROM recent",
            "(SELECT 1) UNION (SELECT 2)",
        ]);
    }

    #[test]
    fn database_administration_queries_require_admin_governance() {
        for query in [
            "SYSTEM SHUTDOWN",
            "KILL QUERY WHERE query_id = 'abc'",
            "OPTIMIZE TABLE events FINAL",
            "ATTACH TABLE events",
            "UNDROP TABLE events",
            "MOVE USER alice TO another_access_storage",
            "BACKUP DATABASE analytics TO Disk('backups', 'analytics.zip')",
            "RESTORE DATABASE analytics FROM Disk('backups', 'analytics.zip')",
        ] {
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Admin,
                "query must require admin governance: {query}"
            );
        }
    }

    #[test]
    fn disruptive_administration_queries_require_destructive_governance() {
        for query in [
            "DETACH TABLE events",
            "RENAME TABLE old TO new",
            "EXCHANGE TABLES old AND new",
        ] {
            assert_eq!(
                classify_sql_execution(query),
                ExecutionClassification::Destructive,
                "query must require destructive governance: {query}"
            );
        }
    }
}
