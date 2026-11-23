//! Approval tools for MCP server.
//!
//! Provides tools for managing execution approvals:
//! - `request_execution`: Request approval for a pending operation
//! - `list_pending_executions`: List all pending executions
//! - `get_pending_execution`: Get details of a specific pending execution
//! - `approve_execution` / `reject_execution`: always denied over MCP by
//!   authorization; a person resolves pending executions in DataTab

use datatab_approval::store::{ExecutionPlan, PendingExecution, PendingStatus};
use datatab_policy::ExecutionClassification;
use rmcp::{
    ErrorData, handler::server::wrapper::Parameters, model::CallToolResult, schemars::JsonSchema,
    tool, tool_router,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{helper::to_json_content, server::DataTabServer};

const DEFAULT_REJECTION_REASON: &str = "rejected by approver";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RequestExecutionParams {
    #[schemars(description = "Tool ID to execute (e.g., 'delete_records', 'drop_table')")]
    pub tool_id: String,

    #[schemars(description = "Connection ID from DataTab configuration")]
    pub connection_id: String,

    #[schemars(description = "Tool parameters as JSON object")]
    pub params: serde_json::Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListPendingExecutionsParams {
    #[schemars(description = "Filter by actor ID (optional)")]
    pub actor_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetPendingExecutionParams {
    #[schemars(description = "Pending execution ID")]
    pub pending_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApproveExecutionParams {
    #[schemars(description = "Pending execution ID")]
    pub pending_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RejectExecutionParams {
    #[schemars(description = "Pending execution ID")]
    pub pending_id: String,

    #[schemars(description = "Reason for rejection (optional)")]
    pub reason: Option<String>,
}

#[tool_router(router = approval_router, vis = "pub")]
impl DataTabServer {
    #[tool(
        description = "Queue an operation for approval by a person in DataTab and return its pending_id. Tell the user to approve it in DataTab (Workspace > Pending Approvals), wait (MCP clients cannot approve), check it with get_pending_execution, and once approved call the target tool itself with exactly the same params to run it once"
    )]
    async fn request_execution(
        &self,
        Parameters(params): Parameters<RequestExecutionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        let client_id = state.client_id.clone();

        let classification = Self::request_execution_classification(&params.tool_id)
            .map_err(|error| ErrorData::invalid_params(error, None))?;

        // Extract connection_id for authorization
        let connection_id_ref = params.connection_id.clone();

        // Clone all values for the closure
        let connection_id = params.connection_id;
        let tool_id = params.tool_id;
        let payload = params.params;

        self.governance
            .authorize_and_execute(
                "request_execution",
                Some(&connection_id_ref),
                classification,
                move || async move {
                    let plan = ExecutionPlan {
                        connection_id,
                        actor_id: client_id,
                        tool_id,
                        classification,
                        payload,
                    };

                    let pending = {
                        let mut runtime = state.runtime.write().await;
                        runtime
                            .request_execution_mut(plan)
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    };

                    let response = serde_json::json!({
                        "pending_id": pending.id,
                        "status": "pending",
                        "classification": format!("{:?}", classification),
                        "message": "Execution request created. Awaiting approval."
                    });

                    Ok(CallToolResult::success(vec![to_json_content(&response)?]))
                },
            )
            .await
    }

    #[tool(
        description = "List executions still waiting for a person to approve or reject them in DataTab. An execution disappears from this list once it is approved, rejected or expired"
    )]
    async fn list_pending_executions(
        &self,
        Parameters(params): Parameters<ListPendingExecutionsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();

        self.governance
            .authorize_and_execute(
                "list_pending_executions",
                None,
                ExecutionClassification::Read,
                move || async move {
                    let pending_list = {
                        let runtime = state.runtime.read().await;
                        let approval_service = runtime.approval_service();
                        approval_service
                            .list_pending()
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    };

                    // Filter by actor_id if provided
                    let filtered: Vec<_> = if let Some(actor_id) = params.actor_id {
                        pending_list
                            .into_iter()
                            .filter(|p| p.plan.actor_id == actor_id)
                            .collect()
                    } else {
                        pending_list
                    };

                    let response = serde_json::json!({
                        "pending_executions": filtered,
                        "count": filtered.len()
                    });

                    Ok(CallToolResult::success(vec![to_json_content(&response)?]))
                },
            )
            .await
    }

    #[tool(
        description = "Check a pending execution by pending_id. While it is returned with status 'pending', it is still waiting for a person to approve it in DataTab. Status 'rejected' means a person rejected it; 'reason' carries what they wrote, if anything: do not repeat the call unchanged. Once it is no longer found it was approved or expired: repeat the identical original call (same tool, same arguments); an approved call then runs once, otherwise a new request is queued"
    )]
    async fn get_pending_execution(
        &self,
        Parameters(params): Parameters<GetPendingExecutionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        let pending_id = params
            .pending_id
            .parse::<Uuid>()
            .map_err(|_| ErrorData::invalid_params("Invalid pending_id format", None))?;

        self.governance
            .authorize_and_execute(
                "get_pending_execution",
                None,
                ExecutionClassification::Read,
                move || async move {
                    let execution = {
                        let runtime = state.runtime.read().await;
                        runtime
                            .approval_service()
                            .get_execution(pending_id)
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    };

                    match execution.and_then(|execution| {
                        pending_execution_status_response(&execution, now_epoch_ms())
                    }) {
                        Some(response) => {
                            Ok(CallToolResult::success(vec![to_json_content(&response)?]))
                        }
                        None => Err(ErrorData::invalid_params(
                            format!("Pending execution not found: {}", pending_id),
                            None,
                        )),
                    }
                },
            )
            .await
    }

    #[tool(
        description = "Always denied over MCP: pending executions are approved by a person in DataTab (Pending Approvals), never by an MCP client"
    )]
    async fn approve_execution(
        &self,
        Parameters(params): Parameters<ApproveExecutionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        let approver_actor_id = state.client_id.clone();
        let pending_id = params
            .pending_id
            .parse::<Uuid>()
            .map_err(|_| ErrorData::invalid_params("Invalid pending_id format", None))?;

        self.governance
            .authorize_and_execute(
                "approve_execution",
                None,
                ExecutionClassification::Admin,
                move || async move {
                    let replay_plan = {
                        let runtime = state.runtime.read().await;
                        runtime
                            .approval_service()
                            .list_pending()
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                            .into_iter()
                            .find(|pending| pending.id == pending_id)
                            .map(|pending| pending.plan)
                            .ok_or_else(|| {
                                ErrorData::invalid_params(
                                    format!("Pending execution not found: {}", pending_id),
                                    None,
                                )
                            })?
                    };

                    {
                        let mut runtime = state.runtime.write().await;
                        runtime
                            .approve_pending_execution_with_origin_mut(
                                &pending_id.to_string(),
                                &approver_actor_id,
                                datatab_core::observability::EventOrigin::mcp(),
                            )
                            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
                    };

                    // Return the approved plan for the caller to execute
                    let tool_id = replay_plan.tool_id.clone();
                    let connection_id = replay_plan.connection_id.clone();
                    let payload = replay_plan.payload.clone();

                    let response = serde_json::json!({
                        "approved": true,
                        "pending_id": pending_id.to_string(),
                        "status": "approved",
                        "replay_plan": {
                            "tool_id": tool_id,
                            "connection_id": connection_id,
                            "params": payload
                        },
                        "message": format!(
                            "Execution approved. Call tool '{}' with the provided params to execute.",
                            replay_plan.tool_id
                        )
                    });

                    Ok(CallToolResult::success(vec![to_json_content(&response)?]))
                },
            )
            .await
    }

    #[tool(
        description = "Always denied over MCP: pending executions are rejected by a person in DataTab (Pending Approvals), never by an MCP client"
    )]
    async fn reject_execution(
        &self,
        Parameters(params): Parameters<RejectExecutionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        let rejection_reason = effective_rejection_reason(params.reason.as_deref()).to_string();
        let pending_id = params
            .pending_id
            .parse::<Uuid>()
            .map_err(|_| ErrorData::invalid_params("Invalid pending_id format", None))?;

        self.governance
            .authorize_and_execute(
                "reject_execution",
                None,
                ExecutionClassification::Admin,
                move || async move {
                    {
                        let mut runtime = state.runtime.write().await;
                        runtime
                            .reject_pending_execution_with_origin_mut(
                                &pending_id.to_string(),
                                &state.client_id,
                                Some(rejection_reason.as_str()),
                                datatab_core::observability::EventOrigin::mcp(),
                            )
                            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
                    }

                    let response = serde_json::json!({
                        "rejected": true,
                        "pending_id": pending_id.to_string(),
                        "reason": rejection_reason
                    });

                    Ok(CallToolResult::success(vec![to_json_content(&response)?]))
                },
            )
            .await
    }

    /// The class an approval request for `tool_id` carries.
    ///
    /// A request is reviewed as at least admin work, because it asks a person to
    /// authorise a call, and it is never less restricted than the call it names:
    /// the two classes are composed, not `max`-ed. `Admin` outranks
    /// `Destructive` on the permission ladder, so `Admin.max(Destructive)` gave
    /// `Admin` and queued a row removal below its own data risk — a policy that
    /// allowed `Admin` and asked on `Destructive` then let it through, and the
    /// approval dialog named the wrong risk (#495). `compose` keeps the
    /// operation's own class on both ladders, so a removal request carries
    /// `AdminDestructive`.
    fn request_execution_classification(tool_id: &str) -> Result<ExecutionClassification, String> {
        let target_classification = Self::classify_tool(tool_id)
            .ok_or_else(|| format!("Unsupported tool for approval request: {}", tool_id))?;

        Ok(ExecutionClassification::Admin.compose(target_classification))
    }

    /// Classify a tool by its ID to determine its execution classification.
    fn classify_tool(tool_id: &str) -> Option<ExecutionClassification> {
        match tool_id {
            // Metadata operations
            "list_connections"
            | "connect"
            | "disconnect"
            | "list_databases"
            | "list_schemas"
            | "list_tables"
            | "list_collections"
            | "describe_object"
            | "get_connection_info"
            | "list_scripts" => Some(ExecutionClassification::Metadata),

            // Read operations
            "select_data" | "count_records" | "aggregate_data" | "explain_query"
            | "preview_mutation" | "get_script" | "query_audit_logs" | "get_audit_entry"
            | "export_audit_logs" => Some(ExecutionClassification::Read),

            // Write operations
            "insert_record" | "update_records" | "upsert_record" | "create_script"
            | "update_script" => Some(ExecutionClassification::Write),

            // Destructive operations
            "delete_records" | "truncate_table" => Some(ExecutionClassification::Destructive),

            // Admin operations
            "create_table" | "create_index" | "create_type" | "delete_script" => {
                Some(ExecutionClassification::Admin)
            }

            // Dynamic or strongly destructive operations default to the stricter class.
            "alter_table" | "execute_script" | "drop_table" | "drop_database" | "drop_index" => {
                Some(ExecutionClassification::AdminDestructive)
            }

            _ => None,
        }
    }
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// What `get_pending_execution` returns for `execution`: the entry itself
/// while it waits for a decision, a rejection payload carrying the person's
/// reason once rejected, and `None` when it was approved, used or expired.
fn pending_execution_status_response(
    execution: &PendingExecution,
    now_ms: i64,
) -> Option<serde_json::Value> {
    match execution.status {
        PendingStatus::Pending if execution.expires_at.is_none_or(|expires| expires > now_ms) => {
            serde_json::to_value(execution).ok()
        }
        PendingStatus::Rejected => Some(serde_json::json!({
            "pending_id": execution.id.to_string(),
            "status": "rejected",
            "tool_id": execution.plan.tool_id,
            "connection_id": execution.plan.connection_id,
            "reason": execution.rejection_reason,
            "message": "A person rejected this execution in DataTab. It will not run. \
                        Repeating the identical call queues a new request.",
        })),
        _ => None,
    }
}

fn effective_rejection_reason(reason: Option<&str>) -> &str {
    reason.unwrap_or(DEFAULT_REJECTION_REASON)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_REJECTION_REASON, DataTabServer, effective_rejection_reason,
        pending_execution_status_response,
    };
    use datatab_approval::store::{ExecutionPlan, PendingExecution, PendingStatus};
    use datatab_policy::ExecutionClassification;

    #[test]
    fn classify_tool_keeps_stricter_requested_execution_levels() {
        assert_eq!(
            DataTabServer::classify_tool("drop_table"),
            Some(ExecutionClassification::AdminDestructive)
        );
        assert_eq!(
            DataTabServer::classify_tool("select_data"),
            Some(ExecutionClassification::Read)
        );
        assert_eq!(
            DataTabServer::classify_tool("alter_table"),
            Some(ExecutionClassification::AdminDestructive)
        );
        assert_eq!(DataTabServer::classify_tool("request_execution"), None);
    }

    #[test]
    fn request_execution_classification_is_at_least_admin() {
        assert_eq!(
            DataTabServer::request_execution_classification("insert_record"),
            Ok(ExecutionClassification::Admin)
        );
        assert_eq!(
            DataTabServer::request_execution_classification("drop_table"),
            Ok(ExecutionClassification::AdminDestructive)
        );
    }

    /// #495: a request for a row removal is queued at the removal's own class,
    /// never at `Admin`, which sits below `Destructive` on the permission
    /// ladder.
    #[test]
    fn request_execution_carries_a_removals_own_data_risk() {
        for tool_id in ["delete_records", "truncate_table"] {
            let request = DataTabServer::request_execution_classification(tool_id)
                .expect("a removal tool is a supported approval target");
            let target = DataTabServer::classify_tool(tool_id)
                .expect("a removal tool has its own classification");

            assert_eq!(
                request,
                ExecutionClassification::AdminDestructive,
                "{tool_id} must be queued at its own data risk"
            );
            assert_ne!(
                request,
                ExecutionClassification::Admin,
                "{tool_id} must never be queued as Admin, below its own data risk"
            );
            assert!(
                request.permits(target),
                "{tool_id}: the request class must permit the class it names"
            );
        }
    }

    /// The whole table in one place: the class a request carries next to the
    /// tool's own class, so a change to either side is visible here.
    #[test]
    fn request_execution_class_pins_the_class_it_carries_per_tool() {
        use ExecutionClassification::*;

        let cases = [
            ("list_tables", Metadata, Admin),
            ("select_data", Read, Admin),
            ("insert_record", Write, Admin),
            ("create_table", Admin, Admin),
            ("delete_records", Destructive, AdminDestructive),
            ("truncate_table", Destructive, AdminDestructive),
            ("drop_table", AdminDestructive, AdminDestructive),
        ];

        for (tool_id, target, expected) in cases {
            assert_eq!(
                DataTabServer::classify_tool(tool_id),
                Some(target),
                "{tool_id} classifies as declared"
            );
            assert_eq!(
                DataTabServer::request_execution_classification(tool_id),
                Ok(expected),
                "{tool_id} must be queued as {expected:?}"
            );
            assert_eq!(
                expected.compose(target),
                expected,
                "{tool_id}: the request class is never raised further by the tool"
            );
        }
    }

    /// #495 from the policy's side: a policy that allows `Admin` and asks on
    /// `Destructive` used to allow a removal request, because it was queued as
    /// `Admin`. It now denies it, because the request carries
    /// `AdminDestructive`, and a policy that asks on that class queues it.
    #[test]
    fn a_policy_that_allows_admin_and_asks_on_destructive_never_allows_a_removal_request() {
        use datatab_policy::{
            ConnectionPolicyAssignment, PolicyBindingScope, PolicyDecision, PolicyDecisionReason,
            PolicyEngine, PolicyEvaluationRequest, ToolPolicy,
        };

        let engine = PolicyEngine::new(
            vec![ConnectionPolicyAssignment {
                actor_id: "actor".to_string(),
                scope: PolicyBindingScope {
                    connection_id: "conn".to_string(),
                },
                role_ids: vec![],
                policy_ids: vec!["admin-allowed".to_string()],
            }],
            vec![],
            vec![ToolPolicy {
                id: "admin-allowed".to_string(),
                allowed_tools: vec!["delete_records".to_string()],
                allowed_classes: vec![ExecutionClassification::Admin],
                approval_classes: vec![ExecutionClassification::Destructive],
            }],
        );

        let evaluate = |classification: ExecutionClassification| {
            engine
                .evaluate(&PolicyEvaluationRequest {
                    actor_id: "actor".to_string(),
                    connection_id: "conn".to_string(),
                    tool_id: "delete_records".to_string(),
                    classification,
                })
                .expect("evaluation must not error")
        };

        // The class the site used to attach: the policy allows the removal.
        assert_eq!(
            evaluate(ExecutionClassification::Admin),
            PolicyDecision::Allow,
            "the old Admin class is what let the removal through"
        );

        assert_eq!(
            evaluate(
                DataTabServer::request_execution_classification("delete_records")
                    .expect("delete_records is a supported approval target")
            ),
            PolicyDecision::Deny(PolicyDecisionReason::ClassificationDenied),
            "a removal request must not be allowed by a policy that only allows Admin"
        );
    }

    /// The same queue, read from the other side: a policy that asks on the
    /// class the request now carries queues the removal instead of allowing it.
    #[test]
    fn a_policy_that_asks_on_the_carried_class_queues_the_removal_request() {
        use datatab_policy::{
            ConnectionPolicyAssignment, PolicyBindingScope, PolicyDecision, PolicyEngine,
            PolicyEvaluationRequest, ToolPolicy,
        };

        let engine = PolicyEngine::new(
            vec![ConnectionPolicyAssignment {
                actor_id: "actor".to_string(),
                scope: PolicyBindingScope {
                    connection_id: "conn".to_string(),
                },
                role_ids: vec![],
                policy_ids: vec!["removal-asks".to_string()],
            }],
            vec![],
            vec![ToolPolicy {
                id: "removal-asks".to_string(),
                allowed_tools: vec!["delete_records".to_string()],
                allowed_classes: vec![ExecutionClassification::Destructive],
                approval_classes: vec![ExecutionClassification::AdminDestructive],
            }],
        );

        let decision = engine
            .evaluate(&PolicyEvaluationRequest {
                actor_id: "actor".to_string(),
                connection_id: "conn".to_string(),
                tool_id: "delete_records".to_string(),
                classification: DataTabServer::request_execution_classification("delete_records")
                    .expect("delete_records is a supported approval target"),
            })
            .expect("evaluation must not error");

        assert_eq!(decision, PolicyDecision::RequireApproval);
    }

    /// #495: the audit row a person reads is written from the class the
    /// request carries, so a removal request's row says `AdminDestructive`.
    #[test]
    fn the_audit_row_names_the_class_the_request_carries() {
        use datatab_core::observability::EventCategory;
        use datatab_core::observability::actions::MCP_AUTHORIZE;
        use datatab_mcp::server::authorization::{
            AuthorizationRequest, authorize_request_with_approval,
        };
        use datatab_mcp::server::request_context::RequestIdentity;
        use datatab_policy::TrustedClient;

        let audit_service = test_audit_service("datatab-approval-class.sqlite");
        let trusted_clients = datatab_policy::TrustedClientRegistry::new(vec![TrustedClient {
            id: "agent-a".to_string(),
            name: "Agent A".to_string(),
            issuer: None,
            active: true,
        }]);
        let policy_engine = datatab_policy::PolicyEngine::default();

        let request = AuthorizationRequest {
            identity: RequestIdentity {
                client_id: "agent-a".to_string(),
                issuer: None,
            },
            connection_id: "conn-a".to_string(),
            tool_id: "request_execution".to_string(),
            classification: DataTabServer::request_execution_classification("delete_records")
                .expect("delete_records is a supported approval target"),
            mcp_enabled_for_connection: true,
            read_only_connection: false,
            correlation_id: None,
        };

        let outcome = authorize_request_with_approval(
            &trusted_clients,
            &policy_engine,
            &audit_service,
            &request,
            None,
            1_000,
        )
        .expect("authorization must not error");
        assert!(!outcome.allowed, "no policy assignment allows the request");

        let rows = audit_service
            .query_extended(&datatab_audit::query::AuditQueryFilter {
                action: Some(MCP_AUTHORIZE.as_str().to_string()),
                category: Some(EventCategory::Mcp.as_str().to_string()),
                ..Default::default()
            })
            .expect("audit query should succeed");

        assert_eq!(rows.len(), 1, "one authorization row is written");
        assert!(
            rows[0]
                .summary
                .as_deref()
                .is_some_and(|summary| summary.contains("classification=AdminDestructive")),
            "the audit row must name the class the request carries, got {:?}",
            rows[0].summary
        );
        assert!(
            rows[0]
                .details_json
                .as_deref()
                .is_some_and(|details| details.contains("\"classification\":\"AdminDestructive\"")),
            "the audit details must name the class the request carries, got {:?}",
            rows[0].details_json
        );
    }

    /// #495: the approval queue entry — what the dialog labels, the bell
    /// shows and the MCP response reports — carries the composed class.
    #[test]
    fn the_queued_request_carries_the_class_the_dialog_labels() {
        use datatab_mcp::{McpGovernanceService as _, McpRuntime};

        let audit_service = test_audit_service("datatab-approval-queue-class.sqlite");
        let mut runtime = McpRuntime::new(
            audit_service,
            Box::new(datatab_approval::InMemoryPendingExecutionStore::default()),
        );

        let queued = runtime
            .request_execution_mut(ExecutionPlan {
                connection_id: "conn-a".to_string(),
                actor_id: "agent-a".to_string(),
                tool_id: "delete_records".to_string(),
                classification: DataTabServer::request_execution_classification("delete_records")
                    .expect("delete_records is a supported approval target"),
                payload: serde_json::json!({ "table": "items" }),
            })
            .expect("queuing a pending execution should succeed");

        assert_eq!(
            queued.classification,
            ExecutionClassification::AdminDestructive
        );

        let pending = runtime
            .list_pending_executions()
            .expect("listing pending executions should succeed");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].classification,
            ExecutionClassification::AdminDestructive,
            "the dialog and the MCP response read this class"
        );
    }

    /// An audit service on a fresh temporary SQLite file, as the MCP
    /// authorization tests use.
    fn test_audit_service(file_name: &str) -> datatab_audit::AuditService {
        use datatab_core::LogErr as _;

        let path = datatab_audit::temp_sqlite_path(file_name);
        std::fs::remove_file(&path).log_err();
        datatab_audit::AuditService::new_sqlite(&path).expect("audit service should initialize")
    }

    #[test]
    fn request_execution_rejects_unsupported_governance_tools() {
        assert!(DataTabServer::request_execution_classification("request_execution").is_err());
        assert!(DataTabServer::request_execution_classification("approve_execution").is_err());
        assert!(DataTabServer::request_execution_classification("unknown_tool").is_err());
    }

    #[test]
    fn request_execution_promotes_read_only_tools_to_admin_review() {
        assert_eq!(
            DataTabServer::request_execution_classification("select_data"),
            Ok(ExecutionClassification::Admin)
        );
    }

    #[test]
    fn reject_execution_uses_runtime_default_reason_when_missing() {
        assert_eq!(effective_rejection_reason(None), DEFAULT_REJECTION_REASON);
    }

    fn execution(status: PendingStatus, rejection_reason: Option<&str>) -> PendingExecution {
        PendingExecution {
            id: uuid::Uuid::new_v4(),
            status,
            plan: ExecutionPlan {
                connection_id: "conn-a".to_string(),
                actor_id: "agent-a".to_string(),
                tool_id: "delete_records".to_string(),
                classification: ExecutionClassification::Destructive,
                payload: serde_json::json!({ "table": "items" }),
            },
            created_at: 1_000,
            expires_at: Some(10_000),
            rejection_reason: rejection_reason.map(str::to_string),
        }
    }

    #[test]
    fn status_response_returns_the_rejection_reason_to_the_agent() {
        let rejected = execution(PendingStatus::Rejected, Some("wrong table"));

        let response = pending_execution_status_response(&rejected, 2_000)
            .expect("a rejected execution is reported");

        assert_eq!(response["status"], serde_json::json!("rejected"));
        assert_eq!(response["reason"], serde_json::json!("wrong table"));
        assert_eq!(
            response["pending_id"],
            serde_json::json!(rejected.id.to_string())
        );
    }

    #[test]
    fn status_response_keeps_pending_and_hides_resolved_or_expired_entries() {
        let pending = execution(PendingStatus::Pending, None);
        let response = pending_execution_status_response(&pending, 2_000)
            .expect("a pending execution is reported");
        assert_eq!(response["status"], serde_json::json!("pending"));

        assert!(pending_execution_status_response(&pending, 20_000).is_none());
        assert!(
            pending_execution_status_response(&execution(PendingStatus::Approved, None), 2_000)
                .is_none()
        );
        assert!(
            pending_execution_status_response(&execution(PendingStatus::Consumed, None), 2_000)
                .is_none()
        );
    }

    #[test]
    fn test_drop_index_classification_admin_destructive() {
        assert_eq!(
            DataTabServer::classify_tool("drop_index"),
            Some(ExecutionClassification::AdminDestructive),
            "drop_index must be classified AdminDestructive, not Admin"
        );
        assert_eq!(
            DataTabServer::classify_tool("create_index"),
            Some(ExecutionClassification::Admin),
            "create_index classification must remain Admin"
        );
    }

    #[test]
    fn test_drop_index_denied_by_admin_destructive_policy() {
        use datatab_policy::{
            ConnectionPolicyAssignment, PolicyBindingScope, PolicyDecision, PolicyDecisionReason,
            PolicyEngine, PolicyEvaluationRequest, ToolPolicy,
        };

        let engine = PolicyEngine::new(
            vec![ConnectionPolicyAssignment {
                actor_id: "actor".to_string(),
                scope: PolicyBindingScope {
                    connection_id: "conn".to_string(),
                },
                role_ids: vec![],
                policy_ids: vec!["admin-only".to_string()],
            }],
            vec![],
            vec![ToolPolicy {
                id: "admin-only".to_string(),
                allowed_tools: vec!["drop_index".to_string()],
                allowed_classes: vec![ExecutionClassification::Admin],
                approval_classes: Vec::new(),
            }],
        );

        let classification = DataTabServer::classify_tool("drop_index")
            .expect("drop_index must have a classification");

        let decision = engine
            .evaluate(&PolicyEvaluationRequest {
                actor_id: "actor".to_string(),
                connection_id: "conn".to_string(),
                tool_id: "drop_index".to_string(),
                classification,
            })
            .expect("evaluation must not error");

        assert_eq!(
            decision,
            PolicyDecision::Deny(PolicyDecisionReason::ClassificationDenied),
            "an Admin-only policy must not authorize drop_index when it is classified AdminDestructive"
        );
    }
}
