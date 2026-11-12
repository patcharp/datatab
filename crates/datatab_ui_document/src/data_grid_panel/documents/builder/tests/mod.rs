#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use datatab_core::{
    CollectionRef, DatabaseCategory, DocumentFeatures, DocumentOperator, DocumentQueryCodec,
    DocumentQueryMode, Pagination,
};
use datatab_driver_mongodb::MongoDocumentCodec;
use gpui::{AppContext, TestAppContext, VisualTestContext};

use super::super::CollectionTab;
use super::BuilderSupport;
use crate::data_grid_panel::tests::register_stub_connection;
use crate::data_grid_panel::{DataGridEvent, DataGridPanel, DataSource};
use crate::document_builder::AccumulatorOp;
use crate::document_builder::DocumentBuilderEvent;

mod rail;
mod slot_sync;

/// A document connection whose builder support is set per test. It
/// answers no query.
pub(in crate::data_grid_panel) struct StubDocumentConnection {
    metadata: datatab_core::DriverMetadata,
    features: DocumentFeatures,
    codec: Option<MongoDocumentCodec>,
    /// Offsets of the collection browses asked for.
    browse_offsets: Arc<Mutex<Vec<u64>>>,
}

impl StubDocumentConnection {
    pub(in crate::data_grid_panel) fn new(
        category: DatabaseCategory,
        features: DocumentFeatures,
        codec: bool,
    ) -> Self {
        Self {
            metadata: datatab_core::DriverMetadata {
                id: "stub-documents".to_string(),
                display_name: "Stub".to_string(),
                description: "test stub".to_string(),
                category,
                transfer_family: datatab_core::TransferFamily::Incompatible,
                deployment_class: None,
                query_language: datatab_core::QueryLanguage::MongoQuery,
                capabilities: datatab_core::DriverCapabilities::empty(),
                default_port: None,
                uri_scheme: "stub".to_string(),
                icon: datatab_core::Icon::Database,
                syntax: None,
                query: None,
                mutation: None,
                ddl: None,
                transactions: None,
                limits: None,
                ssl_modes: None,
                ssl_cert_fields: None,
                classification_override: None,
                default_chunk_size: None,
                supports_lock_timeout: false,
                editor_profile: None,
            },
            features,
            codec: codec.then_some(MongoDocumentCodec),
            browse_offsets: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn logging_browse_offsets(mut self, offsets: Arc<Mutex<Vec<u64>>>) -> Self {
        self.browse_offsets = offsets;
        self
    }
}

impl datatab_core::Connection for StubDocumentConnection {
    /// Facets as a driver of the stub's category declares them.
    fn collection_facets(
        &self,
        _collection: &datatab_core::CollectionRef,
    ) -> Vec<datatab_core::ObjectFacet> {
        datatab_core::category_collection_facets(self.metadata().category)
    }

    fn metadata(&self) -> &datatab_core::DriverMetadata {
        &self.metadata
    }

    fn kind(&self) -> datatab_core::DbKind {
        datatab_core::DbKind::MongoDB
    }

    fn schema_loading_strategy(&self) -> datatab_core::SchemaLoadingStrategy {
        datatab_core::SchemaLoadingStrategy::SingleDatabase
    }

    fn dialect(&self) -> &dyn datatab_core::SqlDialect {
        &datatab_core::DefaultSqlDialect
    }

    fn ping(&self) -> Result<(), datatab_core::DbError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), datatab_core::DbError> {
        Ok(())
    }

    fn execute(
        &self,
        _request: &datatab_core::QueryRequest,
    ) -> Result<datatab_core::QueryResult, datatab_core::DbError> {
        Err(datatab_core::DbError::NotSupported("stub".to_string()))
    }

    fn cancel(&self, _handle: &datatab_core::QueryHandle) -> Result<(), datatab_core::DbError> {
        Ok(())
    }

    fn schema(&self) -> Result<datatab_core::SchemaSnapshot, datatab_core::DbError> {
        Ok(datatab_core::SchemaSnapshot::default())
    }

    fn document_features(&self) -> DocumentFeatures {
        self.features
    }

    fn estimate_collection_count(
        &self,
        _request: &datatab_core::CollectionCountRequest,
    ) -> Result<datatab_core::CollectionCountEstimate, datatab_core::DbError> {
        Ok(datatab_core::CollectionCountEstimate {
            count: 800,
            exact: false,
        })
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "test fixture: the existing panic-on-poison policy is retained for this mutex"
    )]
    fn browse_collection(
        &self,
        request: &datatab_core::CollectionBrowseRequest,
    ) -> Result<datatab_core::QueryResult, datatab_core::DbError> {
        self.browse_offsets
            .lock()
            .unwrap()
            .push(request.pagination.offset());

        Ok(datatab_core::QueryResult::json(
            Vec::new(),
            Vec::new(),
            std::time::Duration::from_millis(1),
        ))
    }

    fn document_query_codec(&self) -> Option<&dyn DocumentQueryCodec> {
        self.codec
            .as_ref()
            .map(|codec| codec as &dyn DocumentQueryCodec)
    }

    fn query_generator(&self) -> Option<&dyn datatab_core::QueryGenerator> {
        Some(&StubGenerator)
    }

    fn aggregate_collection(
        &self,
        _request: &datatab_core::CollectionAggregateRequest,
    ) -> Result<datatab_core::QueryResult, datatab_core::DbError> {
        Ok(datatab_core::QueryResult::json(
            vec![datatab_core::ColumnMeta {
                name: "_id".to_string(),
                type_name: String::new(),
                kind: datatab_core::ColumnKind::Text,
                nullable: true,
                is_primary_key: false,
            }],
            vec![vec![datatab_core::Value::Text("team".to_string())]],
            std::time::Duration::from_millis(1),
        ))
    }
}

/// Native text of a pipeline, so it classifies as a read and runs
/// without asking.
struct StubGenerator;

impl datatab_core::QueryGenerator for StubGenerator {
    fn supported_categories(&self) -> &'static [datatab_core::MutationCategory] {
        &[datatab_core::MutationCategory::Document]
    }

    fn generate_mutation(
        &self,
        _mutation: &datatab_core::MutationRequest,
    ) -> Option<datatab_core::GeneratedQuery> {
        None
    }

    fn aggregate_query(
        &self,
        request: &datatab_core::CollectionAggregateRequest,
    ) -> Option<datatab_core::GeneratedQuery> {
        Some(datatab_core::GeneratedQuery {
            language: datatab_core::QueryLanguage::MongoQuery,
            text: format!(
                "db.{}.aggregate({})",
                request.collection.name,
                serde_json::Value::Array(request.pipeline.clone())
            ),
        })
    }
}

fn builder_features() -> DocumentFeatures {
    DocumentFeatures::QUERY_SLOTS | DocumentFeatures::VISUAL_BUILDER
}

fn aggregate_features() -> DocumentFeatures {
    builder_features() | DocumentFeatures::AGGREGATE
}

fn collection_panel(
    cx: &mut TestAppContext,
    connection: StubDocumentConnection,
) -> (gpui::Entity<DataGridPanel>, &mut VisualTestContext) {
    let (app_state, profile_id) = register_stub_connection(cx, Arc::new(connection));
    let window = cx.add_empty_window();

    let panel = window.update(|window, cx| {
        cx.new(|cx| {
            let source = DataSource::Collection {
                profile_id,
                collection: CollectionRef::new("shop", "orders"),
                pagination: Pagination::default(),
                total_docs: None,
            };
            DataGridPanel::new_internal(source, app_state, vec![], window, cx)
        })
    });
    window.run_until_parked();

    (panel, window)
}

/// Counts the rail events the grid emits from now on.
fn rail_events(
    panel: &gpui::Entity<DataGridPanel>,
    window: &mut VisualTestContext,
) -> Rc<RefCell<Vec<&'static str>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();

    window.update(|_, cx| {
        cx.subscribe(panel, move |_, event: &DataGridEvent, _| match event {
            DataGridEvent::OpenInspector { .. } => sink.borrow_mut().push("open"),
            DataGridEvent::CloseInspector => sink.borrow_mut().push("close"),
            _ => {}
        })
        .detach();
    });

    events
}

fn set_slot_texts(
    panel: &gpui::Entity<DataGridPanel>,
    window: &mut VisualTestContext,
    filter: &str,
    projection: &str,
) {
    window.update(|window, cx| {
        panel.update(cx, |grid, cx| {
            grid.filter_bar
                .filter_input
                .update(cx, |input, cx| input.set_value(filter, window, cx));
            grid.collection
                .projection_input
                .update(cx, |input, cx| input.set_value(projection, window, cx));
        });
    });
}

fn slot_texts(
    panel: &gpui::Entity<DataGridPanel>,
    window: &mut VisualTestContext,
) -> (String, String) {
    window.update(|_, cx| {
        let grid = panel.read(cx);
        (
            grid.filter_bar.filter_input.read(cx).value().to_string(),
            grid.collection
                .projection_input
                .read(cx)
                .value()
                .to_string(),
        )
    })
}

fn open_builder(panel: &gpui::Entity<DataGridPanel>, window: &mut VisualTestContext) {
    window.update(|window, cx| {
        panel.update(cx, |grid, cx| grid.open_document_builder(window, cx));
    });
    window.run_until_parked();
}

fn builder(
    panel: &gpui::Entity<DataGridPanel>,
    window: &mut VisualTestContext,
) -> gpui::Entity<crate::document_builder::DocumentBuilderPanel> {
    window.update(|_, cx| {
        panel
            .read(cx)
            .collection
            .builder
            .panel
            .clone()
            .expect("builder panel")
    })
}

fn first_condition(
    panel: &gpui::Entity<DataGridPanel>,
    window: &mut VisualTestContext,
) -> (u64, String, DocumentOperator) {
    let builder = builder(panel, window);
    window.update(|_, cx| {
        let draft = builder.read(cx).draft();
        let condition = draft.conditions()[0];
        (condition.id, condition.path.clone(), condition.operator)
    })
}

fn set_query_name(
    rail: &gpui::Entity<crate::document_builder::DocumentBuilderPanel>,
    window: &mut VisualTestContext,
    name: &str,
) {
    window.update(|window, cx| {
        rail.update(cx, |rail, cx| rail.set_query_name(name, window, cx));
    });
}

fn stored_queries(
    panel: &gpui::Entity<DataGridPanel>,
    window: &mut VisualTestContext,
) -> Vec<datatab_storage::SavedDocumentQuerySummary> {
    window.update(|_, cx| {
        let grid = panel.read(cx);
        let scope = grid.document_query_scope().expect("a collection tab");
        grid.app_state.clone().update(cx, |app, _| {
            app.saved_document_queries.list(&scope).expect("list")
        })
    })
}

fn sync_slots(panel: &gpui::Entity<DataGridPanel>, window: &mut VisualTestContext) {
    window.update(|_, cx| {
        panel.update(cx, |grid, cx| grid.sync_slots_into_document_builder(cx));
    });
}
