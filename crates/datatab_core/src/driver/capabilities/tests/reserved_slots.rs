use super::super::{DatabaseCategory, QueryLanguage};

/// The script extensions of the removed AWS query languages (#311) are no
/// longer recognised, so callers fall back as for any unknown extension.
#[test]
fn retired_script_extensions_are_not_recognised() {
    for name in ["query.cwli", "query.ppl"] {
        assert_eq!(
            QueryLanguage::from_path(std::path::Path::new(name)),
            None,
            "{name}"
        );
    }
    assert_eq!(
        QueryLanguage::from_path(std::path::Path::new("query.sql")),
        Some(QueryLanguage::Sql)
    );
}

/// A reserved language slot has no dialect of its own: every accessor answers
/// as it does for `QueryLanguage::Custom`.
#[test]
fn reserved_language_slots_behave_like_a_custom_language() {
    let custom = QueryLanguage::Custom("driver dialect".to_string());

    for reserved in [
        QueryLanguage::ReservedCloudWatchLogsInsightsQl,
        QueryLanguage::ReservedOpenSearchPpl,
        QueryLanguage::ReservedOpenSearchSql,
    ] {
        assert_eq!(reserved.context_id(), custom.context_id(), "{reserved:?}");
        assert_eq!(
            reserved.default_extension(),
            custom.default_extension(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.file_dialog_extensions(),
            custom.file_dialog_extensions(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.file_extension(),
            custom.file_extension(),
            "{reserved:?}"
        );
        assert_eq!(reserved.editor_mode(), custom.editor_mode(), "{reserved:?}");
        assert_eq!(reserved.placeholder(), custom.placeholder(), "{reserved:?}");
        assert_eq!(
            reserved.comment_prefix(),
            custom.comment_prefix(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.supports_connection_context(),
            custom.supports_connection_context(),
            "{reserved:?}"
        );
        assert_eq!(reserved.display_name(), "Unknown", "{reserved:?}");
    }
}

/// The category slots retired in #311 answer like the older reserved slots:
/// a driver that declares one gets the generic wording and no
/// category-specific capability.
#[test]
fn retired_category_slots_answer_like_the_other_reserved_slots() {
    let generic = DatabaseCategory::ReservedGraph;

    for reserved in [
        DatabaseCategory::ReservedLogStream,
        DatabaseCategory::ReservedSecretStore,
    ] {
        assert_eq!(
            reserved.display_name(),
            generic.display_name(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.container_name(),
            generic.container_name(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.container_name_singular(),
            generic.container_name_singular(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.record_name(),
            generic.record_name(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.record_name_singular(),
            generic.record_name_singular(),
            "{reserved:?}"
        );
        assert_eq!(
            reserved.relevant_capabilities(),
            generic.relevant_capabilities(),
            "{reserved:?}"
        );
    }
}
