//! Presentation of a [`DatabaseCategory`], kept in one table so that adding a
//! category is a single arm here instead of a `match` in every UI crate.

use datatab_core::DatabaseCategory;

use crate::icons::{AppIcon, DriverIconTone};

/// How the UI presents a database category when no driver-specific value applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CategoryPresentation {
    /// Icon for a driver that declares the generic `Icon::Database`.
    pub fallback_icon: AppIcon,
    /// Logo tone for a driver that declares the generic `Icon::Database`.
    pub fallback_tone: DriverIconTone,
    /// Translation key of the schema-tree container folder, with a `%{count}`
    /// placeholder, e.g. `"Tables (12)"`.
    pub container_label_key: &'static str,
}

pub const fn category_presentation(category: DatabaseCategory) -> CategoryPresentation {
    let (fallback_icon, fallback_tone, container_label_key) = match category {
        // The reserved categories are never declared by a driver; they only
        // keep wire indices, so they borrow the relational look.
        DatabaseCategory::Relational
        | DatabaseCategory::ReservedGraph
        | DatabaseCategory::ReservedWideColumn
        | DatabaseCategory::ReservedLogStream
        | DatabaseCategory::ReservedSecretStore => (
            AppIcon::Database,
            DriverIconTone::Info,
            "sidebar.tree.container.relational",
        ),
        DatabaseCategory::Document => (
            AppIcon::Braces,
            DriverIconTone::Success,
            "sidebar.tree.container.document",
        ),
        DatabaseCategory::KeyValue => (
            AppIcon::KeyRound,
            DriverIconTone::Danger,
            "sidebar.tree.container.key_value",
        ),
        DatabaseCategory::TimeSeries => (
            AppIcon::Database,
            DriverIconTone::Violet,
            "sidebar.tree.container.time_series",
        ),
        DatabaseCategory::ObjectStorage => (
            AppIcon::Boxes,
            DriverIconTone::Warning,
            "sidebar.tree.container.object_storage",
        ),
    };

    CategoryPresentation {
        fallback_icon,
        fallback_tone,
        container_label_key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_CATEGORIES: [DatabaseCategory; 5] = [
        DatabaseCategory::Relational,
        DatabaseCategory::Document,
        DatabaseCategory::KeyValue,
        DatabaseCategory::TimeSeries,
        DatabaseCategory::ObjectStorage,
    ];

    #[test]
    fn container_label_key_resolves() {
        for category in ALL_CATEGORIES {
            let key = category_presentation(category).container_label_key;
            let value = datatab_i18n::t!(key, locale = "en");
            assert!(
                value.contains("%{count}"),
                "{key} must resolve to a template with a count, got {value:?}"
            );
        }
    }

    #[test]
    fn container_label_keys_are_distinct_per_category() {
        let keys: std::collections::HashSet<_> = ALL_CATEGORIES
            .iter()
            .map(|category| category_presentation(*category).container_label_key)
            .collect();
        assert_eq!(keys.len(), ALL_CATEGORIES.len());
    }

    /// A reserved wire slot is no category a driver declares: every one of
    /// them borrows the relational look and container label.
    #[test]
    fn reserved_category_slots_borrow_the_relational_presentation() {
        let relational = category_presentation(DatabaseCategory::Relational);

        for reserved in [
            DatabaseCategory::ReservedGraph,
            DatabaseCategory::ReservedWideColumn,
            DatabaseCategory::ReservedLogStream,
            DatabaseCategory::ReservedSecretStore,
        ] {
            let presentation = category_presentation(reserved);
            assert_eq!(
                presentation.fallback_icon, relational.fallback_icon,
                "{reserved:?}"
            );
            assert_eq!(
                presentation.fallback_tone, relational.fallback_tone,
                "{reserved:?}"
            );
            assert_eq!(
                presentation.container_label_key, relational.container_label_key,
                "{reserved:?}"
            );
        }
    }

    #[test]
    fn fallback_icon_and_tone_keep_the_category_mapping() {
        let expected = [
            (
                DatabaseCategory::Relational,
                AppIcon::Database,
                DriverIconTone::Info,
            ),
            (
                DatabaseCategory::Document,
                AppIcon::Braces,
                DriverIconTone::Success,
            ),
            (
                DatabaseCategory::KeyValue,
                AppIcon::KeyRound,
                DriverIconTone::Danger,
            ),
            (
                DatabaseCategory::TimeSeries,
                AppIcon::Database,
                DriverIconTone::Violet,
            ),
            (
                DatabaseCategory::ObjectStorage,
                AppIcon::Boxes,
                DriverIconTone::Warning,
            ),
        ];

        for (category, icon, tone) in expected {
            let presentation = category_presentation(category);
            assert_eq!(presentation.fallback_icon, icon, "{category:?}");
            assert_eq!(presentation.fallback_tone, tone, "{category:?}");
        }
    }
}
