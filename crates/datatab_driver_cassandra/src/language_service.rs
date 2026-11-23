//! CQL language service: validation, danger detection and execution
//! classification for Cassandra and ScyllaDB.
//!
//! CQL looks like SQL but is not SQL, so the shared SQL heuristic in
//! `datatab_core` would misread it (it has no `Write` default that helps a
//! plain `SELECT`). The service classifies every statement it is given, and
//! `classify_query_for_language_with_service` consults it for
//! `QueryLanguage::Cql`.
//!
//! The detection seam (`LanguageService::detect_dangerous` and
//! `LanguageService::classify_execution`) receives only the statement text, so
//! the one rule it cannot read from text alone is what a `DELETE` will remove.
//! CQL requires a `WHERE` on `DELETE`, which makes every delete a valid
//! statement, but only the table's primary key says whether that `WHERE`
//! names one row, a range of rows or a whole partition. The service therefore
//! keeps the key columns of every table whose details the connection has read,
//! and a `DELETE` is safe only when its `WHERE` constrains all of them with
//! `=`. A table whose key is not known is flagged too: a confirmation costs
//! one click, and a silently removed partition costs the data. A bare table
//! name resolves against the session's active keyspace, which the connection
//! shares with the service, and a schema change the connection ran drops the
//! cached keys so a recreated table is never judged by its old key.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use datatab_core::{
    CQL_BATCH_HEADER_WORDS, DangerousQueryKind, Diagnostic, ExecutionClassification,
    LanguageService, ValidationResult,
};

/// The primary-key columns of each table the connection has read, keyed by
/// keyspace and table as the catalog names them, partition key first and then
/// the clustering columns.
type KeyCache = Arc<RwLock<HashMap<(String, String), Vec<String>>>>;

/// The keyspace the session runs in, held by the connection and read here so a
/// bare table name resolves to the keyspace the statement will run in.
pub(crate) type ActiveKeyspace = Arc<RwLock<Option<String>>>;

/// The language service Cassandra and ScyllaDB connections hand out.
///
/// It holds the primary-key columns of each table whose details or columns the
/// connection has read from `system_schema`, which is what lets a `DELETE` be
/// read as keyed or partial. It shares the connection's active keyspace, so a
/// statement that names a bare table is judged against the keyspace it will
/// run in and no other.
pub struct CqlLanguageService {
    keys: KeyCache,
    active_keyspace: ActiveKeyspace,
}

impl CqlLanguageService {
    /// A service that knows no table's key yet, which is the state a fresh
    /// connection starts in: every `DELETE` confirms until the table's details
    /// are read. `active_keyspace` is the connection's own lock, not a copy.
    pub(crate) fn new(active_keyspace: ActiveKeyspace) -> Self {
        Self {
            keys: KeyCache::default(),
            active_keyspace,
        }
    }

    /// Records the primary-key columns of one table, as the
    /// `system_schema.columns` rows gave them.
    ///
    /// The cache is a hint and never a requirement: a poisoned lock only means
    /// another thread panicked while holding it, and recovering the map keeps
    /// the record instead of failing a schema read over it.
    pub(crate) fn remember_key_columns(&self, keyspace: &str, table: &str, columns: Vec<String>) {
        let mut keys = self
            .keys
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        keys.insert((keyspace.to_string(), table.to_string()), columns);
    }

    /// The primary-key columns of the table a `DELETE` names, or `None` when
    /// this service has not read them.
    ///
    /// A bare table name belongs to the session's keyspace, so it resolves
    /// against that keyspace alone: reading the same table's key in other
    /// keyspaces says nothing about the one the statement runs in. An unknown
    /// active keyspace leaves the name unresolved, which flags the delete
    /// instead of guessing.
    fn key_columns_of(&self, target: &DeleteTarget) -> Option<Vec<String>> {
        let keyspace = match &target.keyspace {
            Some(keyspace) => keyspace.clone(),
            None => self.active_keyspace()?,
        };

        self.keys
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(keyspace, target.table.clone()))
            .cloned()
    }

    /// The primary-key columns of one relation, as a schema read recorded
    /// them, or `None` when this session has not read them.
    ///
    /// The edit path reads this to address a row by its whole key.
    pub(crate) fn key_columns(&self, keyspace: &str, table: &str) -> Option<Vec<String>> {
        self.keys
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&(keyspace.to_string(), table.to_string()))
            .cloned()
    }

    /// The keyspace the session runs in, as the connection recorded it.
    fn active_keyspace(&self) -> Option<String> {
        self.active_keyspace
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Applies what a statement that just ran changed about this cache.
    ///
    /// Two things: the keyspace a `USE` switches the session to, so a bare
    /// table name is judged where the next statement will run; and the keys of
    /// every table, which a schema change may have rewritten. The whole key
    /// cache goes on a schema change rather than one table's entry, because
    /// `DROP KEYSPACE` takes keys whose names this service never learned, and
    /// a stale entry is worse than a cold one: a table dropped and recreated
    /// under a different key would be judged by the old key, and a partial
    /// delete would pass as a full-key one.
    ///
    /// The connection calls this after a statement ran, never from the
    /// classification seams, because the editor classifies what is merely
    /// typed.
    pub(crate) fn statement_ran(&self, query: &str) {
        let tokens = tokenize(query);
        let statements = statements_of(&tokens);

        for statement in &statements {
            if let Some(keyspace) = use_target(statement) {
                self.set_active_keyspace(keyspace);
            }
        }

        if self.changes_a_key(&statements) {
            self.clear_keys();
        }
    }

    /// Applies what a statement that failed may still have changed.
    ///
    /// The server can apply a statement and still answer with an error: a DDL
    /// that timed out, or a write that reached its replicas after the
    /// coordinator gave up waiting. So a failed schema change clears the keys
    /// like a successful one does. The keyspace stays where it was, because a
    /// `USE` that failed moved nothing.
    pub(crate) fn statement_failed(&self, query: &str) {
        let tokens = tokenize(query);
        let statements = statements_of(&tokens);

        if self.changes_a_key(&statements) {
            self.clear_keys();
        }
    }

    /// Whether any statement in `statements` may have rewritten a table's key.
    fn changes_a_key(&self, statements: &[&[Token]]) -> bool {
        statements.iter().any(|statement| {
            matches!(
                self.classify_statement(statement),
                ExecutionClassification::AdminSafe
                    | ExecutionClassification::Admin
                    | ExecutionClassification::AdminDestructive
            )
        })
    }

    /// Drops every cached key.
    fn clear_keys(&self) {
        self.keys
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// Records the keyspace a `USE` switched the session to, in the lock the
    /// connection also reads its active keyspace from.
    fn set_active_keyspace(&self, keyspace: String) {
        let mut active = self
            .active_keyspace
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *active = Some(keyspace);
    }

    /// Whether a `DELETE` names every primary-key column of its table in its
    /// `WHERE` clause with `=`, the one shape that removes a single row.
    ///
    /// A range (`>`, `>=`, `<`, `<=`), `IN`, `CONTAINS`, a slice or a tuple
    /// relation on a key column removes a range of rows or a partition, and so
    /// does leaving a key column out. So does a table this service has not
    /// read, which is the cold-cache case the README states.
    fn deletes_the_full_key(&self, tokens: &[Token]) -> bool {
        let Some(target) = delete_target(tokens) else {
            return false;
        };
        let Some(key_columns) = self.key_columns_of(&target) else {
            return false;
        };
        // CQL requires a primary key, so an empty set means the catalog answer
        // was not what this driver reads; `all` over it would be vacuously
        // true, which would wave the delete through.
        if key_columns.is_empty() {
            return false;
        }

        let equalities = equality_constrained_columns(tokens);
        key_columns
            .iter()
            .all(|key| equalities.iter().any(|column| column == key))
    }

    /// Classifies a CQL buffer: every statement is classified and the result is
    /// never less restricted than any of them, and a buffer of several
    /// statements is at least a write, as the shared SQL heuristic also rules.
    fn classify_cql(&self, query: &str) -> ExecutionClassification {
        let tokens = tokenize(query);
        let statements = statements_of(&tokens);

        if statements.is_empty() {
            return ExecutionClassification::Metadata;
        }

        let classification = statements
            .iter()
            .copied()
            .map(|statement| self.classify_statement(statement))
            .fold(ExecutionClassification::Metadata, |worst, found| {
                worst.compose(found)
            });

        if statements.len() > 1 {
            classification.compose(ExecutionClassification::Write)
        } else {
            classification
        }
    }

    /// Classifies one statement by its leading keyword.
    ///
    /// The statements of a `BEGIN BATCH` block reach here one by one, because
    /// the whole buffer is split at every `;`; `BEGIN` and `APPLY` themselves
    /// fall to the conservative default.
    fn classify_statement(&self, tokens: &[Token]) -> ExecutionClassification {
        let words = words_of(tokens);

        match words.first().copied() {
            None => ExecutionClassification::Metadata,
            Some("SELECT") => {
                if selects_only_system_keyspaces(tokens) {
                    ExecutionClassification::Metadata
                } else {
                    ExecutionClassification::Read
                }
            }
            // `DESCRIBE` is a cqlsh statement, and `USE` only switches the
            // session's keyspace; neither reads or writes user data.
            Some("DESCRIBE" | "DESC" | "USE") => ExecutionClassification::Metadata,
            Some("INSERT" | "UPDATE") => ExecutionClassification::Write,
            // A delete of one row by its whole key is a write; any other shape
            // removes a range of rows or a partition.
            Some("DELETE") => {
                if self.deletes_the_full_key(tokens) {
                    ExecutionClassification::Write
                } else {
                    ExecutionClassification::Destructive
                }
            }
            Some("CREATE") => ExecutionClassification::AdminSafe,
            Some("ALTER") => ExecutionClassification::Admin,
            // Core rates these `Admin` for SQL too; the fallback below would make
            // them a plain `Write`, below `CREATE TABLE`.
            Some("GRANT" | "REVOKE") => ExecutionClassification::Admin,
            Some("DROP") => {
                if words.get(1) == Some(&"INDEX") {
                    ExecutionClassification::Admin
                } else {
                    ExecutionClassification::AdminDestructive
                }
            }
            Some("TRUNCATE") => ExecutionClassification::AdminDestructive,
            Some(_) => ExecutionClassification::Write,
        }
    }

    /// The dangerous kind one statement carries, or `None`.
    fn dangerous_kind_of(&self, tokens: &[Token]) -> Option<DangerousQueryKind> {
        let words = words_of(tokens);

        match words.first().copied()? {
            "TRUNCATE" => Some(DangerousQueryKind::Truncate),
            "DROP" => Some(DangerousQueryKind::Drop),
            "ALTER" => Some(DangerousQueryKind::Alter),
            "DELETE" if !self.deletes_the_full_key(tokens) => {
                Some(DangerousQueryKind::CqlPartialKeyDelete)
            }
            _ => None,
        }
    }
}

impl LanguageService for CqlLanguageService {
    /// Checks the input is non-empty and every `BEGIN BATCH` has its
    /// `APPLY BATCH`.
    fn validate(&self, query: &str) -> ValidationResult {
        if query.trim().is_empty() {
            return ValidationResult::Valid;
        }

        let tokens = tokenize(query);
        let mut open_batches = 0usize;

        for statement in statements_of(&tokens) {
            let words = words_of(statement);

            if opens_batch(&words) {
                open_batches += 1;
            }

            // Read separately from the opening test: one statement can do
            // both, as `BEGIN BATCH APPLY BATCH` does.
            if closes_batch(statement) {
                if open_batches == 0 {
                    return ValidationResult::SyntaxError(
                        Diagnostic::error(
                            "APPLY BATCH closes a batch that BEGIN BATCH never opened",
                        )
                        .with_hint("Remove the APPLY BATCH, or open the batch with BEGIN BATCH"),
                    );
                }
                open_batches -= 1;
            }
        }

        if open_batches > 0 {
            return ValidationResult::SyntaxError(
                Diagnostic::error("BEGIN BATCH has no APPLY BATCH")
                    .with_hint("Close the batch with APPLY BATCH before running it"),
            );
        }

        ValidationResult::Valid
    }

    fn detect_dangerous(&self, query: &str) -> Option<DangerousQueryKind> {
        let tokens = tokenize(query);
        let statements = statements_of(&tokens);

        match statements.as_slice() {
            [] => None,
            [statement] => self.dangerous_kind_of(statement),
            // A buffer of several statements — or one `BEGIN BATCH` block,
            // which splits at its inner `;` — confirms once as a script.
            many => many
                .iter()
                .copied()
                .find_map(|statement| self.dangerous_kind_of(statement))
                .map(|_| DangerousQueryKind::Script),
        }
    }

    fn classify_execution(&self, query: &str) -> Option<ExecutionClassification> {
        Some(self.classify_cql(query))
    }
}

/// The relation a `DELETE` names after its `FROM`, with each name folded the
/// way CQL folds an identifier.
struct DeleteTarget {
    /// `None` when the statement names a bare table: that table belongs to the
    /// session's keyspace, which the statement text does not name.
    keyspace: Option<String>,
    table: String,
}

/// The keyspace and table a statement's `FROM` names.
fn delete_target(tokens: &[Token]) -> Option<DeleteTarget> {
    let first = tokens
        .iter()
        .position(|token| token.word() == Some("FROM"))?
        + 1;
    let name = tokens.get(first)?.identifier()?;

    if tokens
        .get(first + 1)
        .is_some_and(|token| token.is_symbol('.'))
    {
        let table = tokens.get(first + 2)?.identifier()?;
        Some(DeleteTarget {
            keyspace: Some(name),
            table,
        })
    } else {
        Some(DeleteTarget {
            keyspace: None,
            table: name,
        })
    }
}

/// The keyspace a `USE` statement switches the session to, or `None` for any
/// other statement.
///
/// CQL switches the session keyspace when the server runs the statement, and
/// nothing else in the driver learns of it, so the connection asks this after a
/// statement ran to keep the classifier's keyspace true.
fn use_target(statement: &[Token]) -> Option<String> {
    if words_of(statement).first() != Some(&"USE") {
        return None;
    }

    statement.get(1)?.identifier()
}

/// The columns a `WHERE` clause compares with `=`, folded the way CQL folds an
/// identifier.
///
/// A column compared any other way — `IN`, a range, `CONTAINS`, a slice, or a
/// tuple relation such as `(a, b) > (1, 2)` — is left out, which keeps a
/// range delete out of the full-key set.
fn equality_constrained_columns(tokens: &[Token]) -> Vec<String> {
    where_relations(tokens)
        .into_iter()
        .filter(|relation| {
            relation
                .get(1)
                .is_some_and(|operator| operator.is_symbol('='))
        })
        .filter_map(|relation| relation.first().and_then(Token::identifier))
        .collect()
}

/// The `AND`-separated comparisons of a statement's `WHERE` clause.
///
/// The clause ends at the `IF` a conditional statement appends, and a
/// comparison's own parentheses are skipped, so an `IN (…)` list or a tuple
/// term stays inside the comparison it belongs to.
fn where_relations(tokens: &[Token]) -> Vec<&[Token]> {
    let Some(start) = tokens
        .iter()
        .position(|token| token.word() == Some("WHERE"))
    else {
        return Vec::new();
    };

    let mut relations = Vec::new();
    let mut begin = start + 1;
    let mut end = tokens.len();
    let mut depth = 0i32;

    for (index, token) in tokens.iter().enumerate().skip(begin) {
        if token.is_symbol('(') {
            depth += 1;
            continue;
        }
        if token.is_symbol(')') {
            depth -= 1;
            continue;
        }
        if depth != 0 {
            continue;
        }

        match token.word() {
            Some("IF") => {
                end = index;
                break;
            }
            Some("AND") => {
                // `begin` and `index` are indices into `tokens`, so the range
                // is always a slice of the stream.
                relations.extend(tokens.get(begin..index));
                begin = index + 1;
            }
            _ => {}
        }
    }

    if begin < end {
        relations.extend(tokens.get(begin..end));
    }
    relations.retain(|relation| !relation.is_empty());
    relations
}

/// The keyspaces whose tables are the cluster's own catalog. A `SELECT` that
/// reads only these is metadata, not a read of user data.
///
/// The list is explicit: a prefix test would put a user keyspace such as
/// `systems` below `Read`, which understates what the statement does.
const SYSTEM_KEYSPACES: [&str; 8] = [
    "system",
    "system_schema",
    "system_auth",
    "system_distributed",
    "system_distributed_everywhere",
    "system_traces",
    "system_views",
    "system_virtual_schema",
];

/// Whether `keyspace` is one of the cluster's own catalog keyspaces.
fn is_system_keyspace(keyspace: &str) -> bool {
    SYSTEM_KEYSPACES
        .iter()
        .any(|known| keyspace.eq_ignore_ascii_case(known))
}

/// Whether every relation a `SELECT` names sits in a system keyspace, which
/// makes the statement a catalog read (`SELECT * FROM system.local`) rather
/// than a read of user data.
///
/// A relation qualifies only when it is written `keyspace.table`: a bare table
/// name belongs to the current keyspace, which the text cannot name.
fn selects_only_system_keyspaces(tokens: &[Token]) -> bool {
    let mut saw_relation = false;
    let mut index = 0;

    while let Some(token) = tokens.get(index) {
        index += 1;

        if token.word() != Some("FROM") {
            continue;
        }

        let Some(keyspace) = tokens.get(index).and_then(Token::word) else {
            return false;
        };
        let qualified = tokens
            .get(index + 1)
            .is_some_and(|next| next.is_symbol('.'));

        if !qualified || !is_system_keyspace(keyspace) {
            return false;
        }

        saw_relation = true;
    }

    saw_relation
}

/// Whether the statement opens a batch: `BEGIN BATCH`, `BEGIN UNLOGGED BATCH`
/// or `BEGIN COUNTER BATCH`.
fn opens_batch(words: &[&str]) -> bool {
    words.first() == Some(&"BEGIN")
        && words
            .iter()
            .take(CQL_BATCH_HEADER_WORDS)
            .any(|word| *word == "BATCH")
}

/// Whether the statement closes a batch: its last two tokens are `APPLY BATCH`,
/// so a whole batch written without an inner `;` (`BEGIN BATCH APPLY BATCH;`)
/// closes the batch its own first word opened.
///
/// The pair must be adjacent and last: `SELECT apply, batch FROM t` and
/// `UPDATE t SET x = 1 WHERE apply = batch` name the words without closing
/// anything.
fn closes_batch(tokens: &[Token]) -> bool {
    let [.., apply, batch] = tokens else {
        return false;
    };

    apply.word() == Some("APPLY") && batch.word() == Some("BATCH")
}

/// The `;`-separated statements of a token stream, empty ones dropped. A batch
/// splits here at its inner `;` on purpose: its statements are what a batch
/// runs.
fn statements_of(tokens: &[Token]) -> Vec<&[Token]> {
    tokens
        .split(|token| token.is_symbol(';'))
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// How many statements the service splits `query` into.
///
/// Test-only: it pins the splitter and the service to one set of CQL lexical
/// rules (issue #472, ruling 6).
#[cfg(test)]
pub(crate) fn statement_count(query: &str) -> usize {
    statements_of(&tokenize(query)).len()
}

/// The unquoted words of a statement, in order, uppercased.
fn words_of(tokens: &[Token]) -> Vec<&str> {
    tokens.iter().filter_map(Token::word).collect()
}

/// One token of a CQL statement.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// An unquoted word, uppercased.
    Word(String),
    /// A double-quoted identifier, with its doubled quotes resolved and its
    /// case kept as written.
    Quoted(String),
    /// A string literal, `'…'` or `$$…$$`: its content is never a keyword and
    /// never a name.
    Literal,
    /// Punctuation, such as `.` or `;`.
    Symbol(char),
}

impl Token {
    fn word(&self) -> Option<&str> {
        match self {
            Token::Word(word) => Some(word),
            Token::Quoted(_) | Token::Literal | Token::Symbol(_) => None,
        }
    }

    /// The name this token spells: an unquoted word folded to lower case, as
    /// CQL folds it, and a quoted identifier kept as written.
    fn identifier(&self) -> Option<String> {
        match self {
            Token::Word(word) => Some(word.to_ascii_lowercase()),
            Token::Quoted(name) => Some(name.clone()),
            Token::Literal | Token::Symbol(_) => None,
        }
    }

    fn is_symbol(&self, symbol: char) -> bool {
        *self == Token::Symbol(symbol)
    }
}

/// Splits a CQL buffer into words and symbols, skipping comments, string
/// literals and quoted identifiers.
///
/// The rules are the ones `datatab_core`'s `StatementLexRules::CQL` applies to
/// a CQL buffer, so the splitter and this lexer cut a buffer into the same
/// lexical pieces: a quote, comment or dollar body moves no `;` for one reader
/// and not the other. Comments are `--` and `//` to the end of the line and
/// `/* … */` without nesting; a quote inside a literal or a quoted identifier
/// is escaped by doubling it and a backslash escapes nothing; `$$…$$` is a
/// quoted string and `$tag$` is not; a backtick quotes nothing. Change one,
/// change both.
fn tokenize(query: &str) -> Vec<Token> {
    let chars: Vec<char> = query.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while let Some(&current) = chars.get(index) {
        let next = chars.get(index + 1).copied();

        if current.is_whitespace() {
            index += 1;
            continue;
        }

        if (current == '-' && next == Some('-')) || (current == '/' && next == Some('/')) {
            while chars.get(index).is_some_and(|&character| character != '\n') {
                index += 1;
            }
            continue;
        }

        if current == '/' && next == Some('*') {
            index += 2;
            while let Some(&character) = chars.get(index) {
                if character == '*' && chars.get(index + 1) == Some(&'/') {
                    index += 2;
                    break;
                }
                index += 1;
            }
            continue;
        }

        if current == '\'' || current == '"' {
            index += 1;
            let mut text = String::new();
            while let Some(&inner) = chars.get(index) {
                if inner == current {
                    if chars.get(index + 1) == Some(&current) {
                        // Doubling a quote is how CQL spells the quote itself.
                        text.push(inner);
                        index += 2;
                        continue;
                    }
                    index += 1;
                    break;
                }
                text.push(inner);
                index += 1;
            }
            tokens.push(if current == '"' {
                Token::Quoted(text)
            } else {
                Token::Literal
            });
            continue;
        }

        // `$$…$$` is a quoted string in CQL, so a `;` inside it separates
        // nothing.
        if current == '$' && next == Some('$') {
            index += 2;
            while let Some(&inner) = chars.get(index) {
                if inner == '$' && chars.get(index + 1) == Some(&'$') {
                    index += 2;
                    break;
                }
                index += 1;
            }
            tokens.push(Token::Literal);
            continue;
        }

        if current.is_alphanumeric() || current == '_' {
            let mut word = String::new();
            while let Some(&character) = chars.get(index) {
                if character.is_alphanumeric() || character == '_' {
                    word.push(character.to_ascii_uppercase());
                    index += 1;
                } else {
                    break;
                }
            }
            tokens.push(Token::Word(word));
            continue;
        }

        tokens.push(Token::Symbol(current));
        index += 1;
    }

    tokens
}
