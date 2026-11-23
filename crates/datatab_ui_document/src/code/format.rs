//! Reformatting the buffer through the document's language service.
//!
//! The Format command, its chord, its palette entry, its pane-action entry and
//! the toolbar button all reach [`CodeDocument::format_sql`]. The text itself
//! is rewritten by [`datatab_core::LanguageService::format`], so a language
//! whose driver ships no formatter keeps the command unavailable instead of
//! rewriting the buffer with rules that do not fit it.

use super::comment::{char_boundary, normalize_selection, utf16_range};
use super::*;
use datatab_core::LanguageService;
use std::ops::Range;

/// One formatted buffer, ready to be applied as a single edit.
pub(super) struct BufferFormat {
    /// Byte range of the current buffer the edit replaces.
    pub range: Range<usize>,
    /// The replacement text for `range`.
    pub text: String,
    /// What to select afterwards, in offsets of the rewritten buffer. A
    /// collapsed range is a caret.
    pub selection: Range<usize>,
}

/// Reformat the statements `selection` touches, leaving the text between them
/// as it was.
///
/// An empty selection formats the whole buffer. `None` means the buffer needs
/// no change — it is already formatted, or the selection touches no statement
/// — so the caller must leave both the document and its undo stack alone.
pub(super) fn format_selection(
    service: &dyn LanguageService,
    language: &QueryLanguage,
    text: &str,
    selection: Range<usize>,
) -> Option<BufferFormat> {
    let statements = statement_regions(language, text)?;
    let (start, end) = normalize_selection(selection, text.len());
    let span = format_span(&statements, start, end, text.len())?;

    // The statements the edit covers, and their positions in the rewritten
    // text, are parallel slices: every index below is into both or neither.
    let in_span: Vec<Range<usize>> = statements
        .into_iter()
        .filter(|range| range.start >= span.start && range.end <= span.end)
        .collect();

    let mut replacement = String::with_capacity(span.len());
    let mut rewritten: Vec<Range<usize>> = Vec::with_capacity(in_span.len());
    let mut cursor = span.start;

    for statement in &in_span {
        replacement.push_str(&text[cursor..statement.start]);
        let new_start = span.start + replacement.len();

        match service.format(text, statement.clone()) {
            Some(edit) => replacement.push_str(&edit.new_text),
            None => replacement.push_str(&text[statement.clone()]),
        }
        cursor = statement.end;

        rewritten.push(new_start..span.start + replacement.len());
    }
    replacement.push_str(&text[cursor..span.end]);

    if replacement == text[span.clone()] {
        return None;
    }

    let selection = if start == end {
        let caret = remap_caret(start, &span, &in_span, &rewritten, &replacement);
        caret..caret
    } else {
        // The formatted statements stay selected, so a second press formats
        // exactly the same text again.
        span.start..span.start + replacement.len()
    };

    Some(BufferFormat {
        range: span,
        text: replacement,
        selection,
    })
}

/// The buffer's statements, trimmed. A language with a formatter but no
/// statement splitter formats its whole buffer as one statement. `None` for a
/// buffer with nothing to format.
fn statement_regions(language: &QueryLanguage, text: &str) -> Option<Vec<Range<usize>>> {
    if let Some(ranges) = language.statement_ranges(text)
        && !ranges.is_empty()
    {
        return Some(ranges);
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let start = text.len() - text.trim_start().len();
    Some(std::iter::once(start..start + trimmed.len()).collect())
}

/// The part of the buffer the edit replaces: every statement from the first
/// to the last one the selection touches, so the text between them can be
/// kept as it was. `None` when the selection touches no statement, and there
/// is nothing to format.
fn format_span(
    statements: &[Range<usize>],
    start: usize,
    end: usize,
    len: usize,
) -> Option<Range<usize>> {
    if start == end {
        return Some(0..len);
    }

    let mut touched = statements
        .iter()
        .filter(|range| range.end > start && range.start < end);
    let first = touched.next()?;
    let last = touched.next_back().unwrap_or(first);

    Some(first.start..last.end)
}

/// Where the caret lands after the edit: the same offset inside the statement
/// it was in, which is what keeps the cursor on that statement while the
/// statements around it are rewritten.
///
/// `statements` and `rewritten` are parallel, one entry per statement the edit
/// covers — not a list of the buffer's statements — so an index into one is an
/// index into the other.
pub(super) fn remap_caret(
    caret: usize,
    span: &Range<usize>,
    statements: &[Range<usize>],
    rewritten: &[Range<usize>],
    replacement: &str,
) -> usize {
    debug_assert_eq!(
        statements.len(),
        rewritten.len(),
        "the span's statements and their rewritten ranges are parallel"
    );

    if caret <= span.start {
        return caret;
    }

    if caret >= span.end {
        return span.start + replacement.len() + (caret - span.end);
    }

    let statement = statements
        .iter()
        .position(|range| caret >= range.start && caret <= range.end);

    let Some(index) = statement else {
        // In the text between statements, which the edit keeps as it was:
        // land on the start of the statement that follows it.
        return statements
            .iter()
            .position(|range| range.start > caret)
            .and_then(|next| rewritten.get(next))
            .map_or(span.start + replacement.len(), |range| range.start);
    };

    let (Some(statement), Some(new_range)) = (statements.get(index), rewritten.get(index)) else {
        return caret;
    };

    let new_statement = &replacement[new_range.start - span.start..new_range.end - span.start];
    let local = char_boundary(new_statement, caret.saturating_sub(statement.start));

    new_range.start + local
}

/// The service that formats a document's text: the connection's own when the
/// document follows that connection and its service has a formatter,
/// otherwise the core's formatter for the document's own language.
///
/// A pinned language did not come from the connection — a file extension, an
/// in-process script language, a routine body — so the connection's service
/// speaks something else and must not rewrite it: `report.cql` opened on a SQL
/// connection formats as nothing, not as SQL.
///
/// The fallback to the core's formatter matters because a driver may replace
/// the shared SQL service to speak its own dialect (MySQL, T-SQL) without also
/// carrying a formatter; those tabs are still SQL, and the shared formatter
/// fits them. A language the core does not format — anything but SQL — gets
/// nothing, so its Format command stays unavailable.
pub(super) fn formatter_for<'a>(
    binding: LanguageBinding,
    connection: Option<&'a dyn LanguageService>,
    language: &QueryLanguage,
) -> Option<&'a dyn LanguageService> {
    if binding == LanguageBinding::FollowsConnection
        && let Some(service) = connection
        && service.can_format()
    {
        return Some(service);
    }

    datatab_core::default_language_service(language)
}

impl CodeDocument {
    /// The language service that formats this document's text.
    ///
    /// `None` when the document's language has no formatter, or the document
    /// is not editable.
    pub(super) fn formatter<'a>(&self, cx: &'a App) -> Option<&'a dyn LanguageService> {
        if self.read_only {
            return None;
        }

        let binding = self.editor.language_binding;
        let connection = match binding {
            LanguageBinding::FollowsConnection => self
                .connection_id
                .and_then(|id| self.app_state.read(cx).connections().get(&id))
                .map(|connected| connected.connection.language_service()),
            LanguageBinding::Pinned => None,
        };

        formatter_for(binding, connection, self.effective_language())
    }

    /// Whether the Format command can rewrite this document.
    pub(super) fn can_format(&self, cx: &App) -> bool {
        self.formatter(cx)
            .is_some_and(|service| service.can_format())
    }

    /// Reformat the buffer, or the statements the selection touches.
    ///
    /// One edit, so one undo step restores the buffer. Returns `false` when
    /// the document is read-only or its language has no formatter; a buffer
    /// that needs no change is reported as handled, with no edit at all.
    pub(super) fn format_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.can_format(cx) {
            if !self.read_only {
                Toast::warning(datatab_i18n::t!(
                    "document.code.toolbar.formatter_unavailable"
                ))
                .meta_right(now_hms())
                .push(cx);
            }
            return false;
        }

        let (selection, text) = {
            let state = self.editor.input_state.read(cx);
            (state.selected_range(), state.value().to_string())
        };
        let language = self.effective_language().clone();

        let formatted = {
            let Some(service) = self.formatter(cx) else {
                return false;
            };
            format_selection(service, &language, &text, selection)
        };

        // Nothing needs formatting: no edit, so the undo stack stays as it was.
        let Some(formatted) = formatted else {
            return true;
        };

        let region = utf16_range(&text, &formatted.range);
        let next_selection = formatted.selection;

        // A format is not a keystroke: the Change handler's deletion path moves
        // the cursor and reopens the completion menu. The flag keeps that half
        // of the bookkeeping out of the way, and the handler consumes it — GPUI
        // delivers `emit` after the outermost update, so clearing it here would
        // be too early. Closing an open menu must not be skipped with it, so
        // that half of the cursor move happens here, before the edit.
        self.editor.programmatic_edit = true;
        self.editor.input_state.update(cx, |state, cx| {
            let scroll_offset = state.scroll_offset();
            let cursor = state.cursor_position();
            state.set_cursor_position(cursor, window, cx);
            state.replace_text_in_range(Some(region), &formatted.text, window, cx);
            state.set_selected_range(next_selection, cx);
            state.set_scroll_offset(scroll_offset, cx);
        });

        true
    }
}
