//! Server-side paging for a table browse.
//!
//! CQL has no `OFFSET`, so the grid's page numbers are served from the paging
//! state the server hands out: page N starts where the server stopped after
//! page N-1 ended. A `LIMIT` is never put on the statement, because a CQL
//! `LIMIT` bounds the whole query — a `LIMIT 100` would empty page 2.
//!
//! The state a page starts at is kept per relation, filter and page size for
//! the life of one connection. Next, previous and a refresh of a page then
//! reuse it, and a forward jump walks the pages in between once.

use std::collections::{HashMap, VecDeque};

use datatab_core::{DbError, Value};
use scylla::response::PagingState;

/// Page start positions one connection keeps.
///
/// A paging state is a small server token (a partition and a clustering key,
/// often a few dozen bytes). The bound is the number of page boundaries kept:
/// 64 covers a session's paging back and forth many times over, and a forward
/// jump past the newest one costs one walk that refills the cache.
pub(crate) const MAX_CACHED_PAGES: usize = 64;

/// Pages a forward jump may read through before it gives up.
///
/// The offset comes from the caller, and `u64::MAX` is a legal offset, so the
/// walk needs a ceiling: a jump that would read a thousand pages of the table
/// fails with a clear error instead of reading for minutes.
pub(crate) const MAX_PAGES_WALKED: u64 = 1_000;

/// Server pages asked for while filling one grid page before the driver calls
/// the answer broken.
///
/// A server page that carries no row but a continuation would repeat forever.
/// A real server does not do it; the bound turns a server bug into an error.
const MAX_EMPTY_SERVER_PAGES: u32 = 16;

/// What one table browse pages over: the relation, the filter that narrows it,
/// the projected columns and the grid's page size.
///
/// A paging state belongs to the exact statement that produced it, so a changed
/// filter or page size starts a new walk instead of reusing a state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BrowseKey {
    keyspace: Option<String>,
    table: String,
    filter: Option<String>,
    columns: Vec<String>,
    limit: u32,
}

impl BrowseKey {
    pub(crate) fn new(
        keyspace: Option<String>,
        table: String,
        filter: Option<String>,
        columns: Vec<String>,
        limit: u32,
    ) -> Self {
        Self {
            keyspace,
            table,
            filter,
            columns,
            limit,
        }
    }
}

/// The page start positions of one connection, oldest evicted first.
#[derive(Debug, Default)]
pub(crate) struct PagingCache {
    entries: HashMap<(BrowseKey, u64), PagingState>,
    order: VecDeque<(BrowseKey, u64)>,
}

impl PagingCache {
    /// The state `page` starts at, or the highest page below it that is kept.
    ///
    /// The walk starts from the nearer of the two, so a forward jump reads only
    /// the pages between the one that is kept and the one asked for.
    pub(crate) fn start_for(&self, key: &BrowseKey, page: u64) -> Option<(u64, PagingState)> {
        self.entries
            .iter()
            .filter(|((entry_key, entry_page), _)| entry_key == key && *entry_page <= page)
            .map(|((_, entry_page), state)| (*entry_page, state.clone()))
            .max_by_key(|(entry_page, _)| *entry_page)
    }

    /// Keeps the state `page` starts at. Page 0 is the start of the scan and
    /// needs no entry; `PagingState::start()` restores it.
    pub(crate) fn insert(&mut self, key: &BrowseKey, page: u64, state: PagingState) {
        if page == 0 {
            return;
        }
        let slot = (key.clone(), page);
        if self.entries.insert(slot.clone(), state).is_some() {
            // A refreshed entry keeps its place in the eviction order.
            return;
        }
        self.order.push_back(slot);
        while self.order.len() > MAX_CACHED_PAGES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    /// Entries kept; the bound is what a long session is judged by.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// One grid page: its rows and the state the next page starts at (`None` when
/// the table has no more rows).
#[derive(Debug)]
pub(crate) struct PageRead {
    pub(crate) rows: Vec<Vec<Value>>,
    pub(crate) next: Option<PagingState>,
}

/// Reads one grid page, starting from a page whose state is known.
///
/// `start_page` and `start_state` are a page boundary the caller has (page 0 is
/// `PagingState::start()`). Pages before `target_page` are read and dropped, and
/// every page boundary passed is reported to `remember`, so the next call can
/// start closer. A page the server ends early is filled with the rows that
/// follow: the grid shows `limit` rows whenever the table has them.
///
/// The fetch is a closure rather than a session call so the walk is testable
/// without a cluster: it is asked for a page size and a state, and answers with
/// the rows and the state after them.
pub(crate) fn read_grid_page<F, R>(
    start_page: u64,
    start_state: PagingState,
    target_page: u64,
    limit: usize,
    fetch: &mut F,
    remember: &mut R,
) -> Result<PageRead, DbError>
where
    F: FnMut(i32, PagingState) -> Result<(Vec<Vec<Value>>, Option<PagingState>), DbError>,
    R: FnMut(u64, PagingState),
{
    if target_page < start_page {
        return Err(DbError::query_failed(
            "The paging cache answered with a page after the one asked for",
        ));
    }
    let Some(limit) = page_size(limit) else {
        return Err(DbError::query_failed(
            "A Cassandra browse needs a page size between 1 and 2147483647 rows",
        ));
    };

    let mut page = start_page;
    let mut state = start_state;

    while page < target_page {
        if target_page - page > MAX_PAGES_WALKED {
            return Err(DbError::NotSupported(format!(
                "Cassandra cannot jump more than {MAX_PAGES_WALKED} pages ahead without reading the pages in between"
            )));
        }
        let (_, next) = fill_grid_page(limit, state, fetch)?;
        let Some(next) = next else {
            // The scan ended before the page asked for: the table has no rows
            // left, so that page is empty.
            return Ok(PageRead {
                rows: Vec::new(),
                next: None,
            });
        };
        page += 1;
        remember(page, next.clone());
        state = next;
    }

    let (rows, next) = fill_grid_page(limit, state, fetch)?;
    Ok(PageRead { rows, next })
}

/// `limit` as a CQL page size, or `None` when it cannot be one.
pub(crate) fn page_size(limit: usize) -> Option<i32> {
    if limit == 0 {
        return None;
    }
    i32::try_from(limit).ok()
}

/// Fills one grid page, asking the server for the rows still missing.
fn fill_grid_page<F>(
    limit: i32,
    mut state: PagingState,
    fetch: &mut F,
) -> Result<(Vec<Vec<Value>>, Option<PagingState>), DbError>
where
    F: FnMut(i32, PagingState) -> Result<(Vec<Vec<Value>>, Option<PagingState>), DbError>,
{
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut empty_pages = 0u32;
    let wanted = limit as usize;

    loop {
        let remaining = wanted.saturating_sub(rows.len());
        if remaining == 0 {
            return Ok((rows, Some(state)));
        }
        let asked = i32::try_from(remaining).map_err(|_| {
            DbError::query_failed("A Cassandra browse page is too large to ask the server for")
        })?;
        let (page_rows, next) = fetch(asked, state.clone())?;
        let page_was_empty = page_rows.is_empty();
        rows.extend(page_rows);
        match next {
            Some(next) => {
                state = next;
                if page_was_empty {
                    empty_pages += 1;
                    if empty_pages > MAX_EMPTY_SERVER_PAGES {
                        return Err(DbError::query_failed(
                            "The server kept answering with an empty page without ending the scan",
                        ));
                    }
                } else {
                    empty_pages = 0;
                }
            }
            None => return Ok((rows, None)),
        }
    }
}
