use super::SchemaNodeKind;

impl SchemaNodeKind {
    pub fn needs_click_handler(&self) -> bool {
        matches!(
            self,
            Self::Profile
                | Self::DatabasesFolder
                | Self::EmptyDatabasesFolder
                | Self::Database
                | Self::Table
                | Self::View
                | Self::Collection
                | Self::ConnectionFolder
                | Self::Schema
                | Self::TablesFolder
                | Self::ViewsFolder
                | Self::TypesFolder
                | Self::ColumnsFolder
                | Self::IndexesFolder
                | Self::ForeignKeysFolder
                | Self::ConstraintsFolder
                | Self::StorageHintsFolder
                | Self::SchemaIndexesFolder
                | Self::SchemaForeignKeysFolder
                | Self::RoutinesFolder
                | Self::CollectionsFolder
                | Self::CollectionFieldsFolder
                | Self::CustomType
                | Self::ScriptsFolder
                | Self::ScriptFile
                | Self::ScriptsRoot
                | Self::DependentsFolder
                | Self::Routine
                | Self::DashboardsFolder
                | Self::DashboardItem
                | Self::SavedChartsFolder
                | Self::SavedChartItem
                | Self::InstanceMetricsFolder
                | Self::InstanceMetricLeaf
                | Self::InstanceInspectorsFolder
                | Self::InstanceFolder
                | Self::InstanceInspectorLeaf
                | Self::InstanceOverviewLeaf
                | Self::RootSection
                | Self::RootSectionItem
                | Self::RootSectionLoadMore
        )
    }

    pub fn is_expandable_folder(&self) -> bool {
        matches!(
            self,
            Self::ConnectionFolder
                | Self::DatabasesFolder
                | Self::EmptyDatabasesFolder
                | Self::Schema
                | Self::TablesFolder
                | Self::ViewsFolder
                | Self::TypesFolder
                | Self::ColumnsFolder
                | Self::IndexesFolder
                | Self::ForeignKeysFolder
                | Self::ConstraintsFolder
                | Self::StorageHintsFolder
                | Self::SchemaIndexesFolder
                | Self::SchemaForeignKeysFolder
                | Self::RoutinesFolder
                | Self::CollectionsFolder
                | Self::CollectionFieldsFolder
                | Self::Database
                | Self::CustomType
                | Self::ScriptsFolder
                | Self::ScriptsRoot
                | Self::DependentsFolder
                | Self::DashboardsFolder
                | Self::SavedChartsFolder
                | Self::InstanceMetricsFolder
                | Self::InstanceInspectorsFolder
                | Self::InstanceFolder
                | Self::RootSection
        )
    }

    pub fn shows_pointer_cursor(&self) -> bool {
        matches!(
            self,
            Self::Profile
                | Self::Database
                | Self::ConnectionFolder
                | Self::ScriptFile
                | Self::Routine
                | Self::DashboardItem
                | Self::SavedChartItem
                | Self::InstanceMetricLeaf
                | Self::InstanceInspectorLeaf
                | Self::InstanceOverviewLeaf
                | Self::RootSection
                | Self::RootSectionItem
                | Self::RootSectionLoadMore
        )
    }
}
