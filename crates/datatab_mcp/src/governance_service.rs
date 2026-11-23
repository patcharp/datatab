use datatab_policy::{ConnectionPolicyAssignment, ExecutionClassification, PolicyRole, ToolPolicy};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedClientDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub issuer: Option<String>,
    #[serde(default)]
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRoleDto {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_ids: Vec<String>,
}

impl From<PolicyRole> for PolicyRoleDto {
    fn from(role: PolicyRole) -> Self {
        Self {
            id: role.id,
            policy_ids: role.policy_ids,
        }
    }
}

impl From<PolicyRoleDto> for PolicyRole {
    fn from(dto: PolicyRoleDto) -> Self {
        Self {
            id: dto.id,
            policy_ids: dto.policy_ids,
        }
    }
}

/// A tool policy as exchanged with the UI and persistence. A class listed in
/// `allowed_classes` is Allow, a class listed in `approval_classes` requires
/// approval (Ask), and a class in neither is Deny.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPolicyDto {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approval_classes: Vec<String>,
}

/// The persisted id of an execution class, as stored in policies.
pub fn execution_class_id(classification: ExecutionClassification) -> &'static str {
    match classification {
        ExecutionClassification::Metadata => "metadata",
        ExecutionClassification::Read => "read",
        ExecutionClassification::Write => "write",
        ExecutionClassification::Destructive => "destructive",
        ExecutionClassification::Admin => "admin",
        ExecutionClassification::AdminSafe => "admin_safe",
        ExecutionClassification::AdminDestructive => "admin_destructive",
    }
}

/// Parses a persisted execution class id.
pub fn parse_execution_class_id(id: &str) -> Result<ExecutionClassification, String> {
    match id {
        "read" => Ok(ExecutionClassification::Read),
        "write" => Ok(ExecutionClassification::Write),
        "destructive" => Ok(ExecutionClassification::Destructive),
        "admin" => Ok(ExecutionClassification::Admin),
        "metadata" => Ok(ExecutionClassification::Metadata),
        "admin_safe" => Ok(ExecutionClassification::AdminSafe),
        "admin_destructive" => Ok(ExecutionClassification::AdminDestructive),
        _ => Err(format!("invalid classification: {}", id)),
    }
}

impl From<ToolPolicy> for ToolPolicyDto {
    fn from(policy: ToolPolicy) -> Self {
        let class_ids = |classes: &[ExecutionClassification]| {
            classes
                .iter()
                .map(|class| execution_class_id(*class).to_string())
                .collect()
        };

        Self {
            allowed_classes: class_ids(&policy.allowed_classes),
            approval_classes: class_ids(&policy.approval_classes),
            id: policy.id,
            allowed_tools: policy.allowed_tools,
        }
    }
}

impl TryFrom<ToolPolicyDto> for ToolPolicy {
    type Error = String;

    fn try_from(dto: ToolPolicyDto) -> Result<Self, Self::Error> {
        let parse_all = |ids: &[String]| {
            ids.iter()
                .map(|id| parse_execution_class_id(id))
                .collect::<Result<Vec<_>, _>>()
        };

        let policy = Self {
            allowed_classes: parse_all(&dto.allowed_classes)?,
            approval_classes: parse_all(&dto.approval_classes)?,
            id: dto.id,
            allowed_tools: dto.allowed_tools,
        };

        policy
            .validate_class_sets()
            .map_err(|error| error.to_string())?;

        Ok(policy)
    }
}

/// Refuses a policy whose class sets grant a class while a class below it on
/// the permission ladder is decided more strictly (#490).
///
/// Saving a policy runs this so the person who wrote it is told the offending
/// pair, and [`TryFrom<ToolPolicyDto> for ToolPolicy`] runs the same check when
/// a stored policy is read back, so the engine never acts on an incoherent
/// policy that reached storage some other way.
pub fn validate_policy_class_sets(policy: &ToolPolicyDto) -> Result<(), String> {
    ToolPolicy::try_from(policy.clone()).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionPolicyAssignmentDto {
    pub connection_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignments: Vec<ConnectionPolicyAssignment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingExecutionSummary {
    pub id: String,
    pub actor_id: String,
    pub connection_id: String,
    pub tool_id: String,
    pub classification: ExecutionClassification,
    pub status: String,
    pub created_at_epoch_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingExecutionDetail {
    pub summary: PendingExecutionSummary,
    pub plan: serde_json::Value,
}

/// Lightweight outcome returned when an approval or rejection completes.
/// Replaces the former `AuditEntry` in approve/reject return positions so
/// that the governance trait no longer depends on audit-query DTOs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalOutcome {
    pub id: String,
    pub status: String,
    pub actor_id: String,
    pub timestamp_ms: i64,
}

#[derive(Debug, Error)]
pub enum GovernanceError {
    #[error("resource not found: {resource}")]
    NotFound { resource: String },
    #[error("validation error: {0}")]
    Validation(String),
    #[error("operation failed: {0}")]
    Operation(String),
}

pub trait McpGovernanceService {
    fn list_trusted_clients(&self) -> Result<Vec<TrustedClientDto>, GovernanceError>;

    fn upsert_trusted_client(
        &self,
        client: TrustedClientDto,
    ) -> Result<TrustedClientDto, GovernanceError>;

    fn delete_trusted_client(&self, client_id: &str) -> Result<(), GovernanceError>;

    fn list_roles(&self) -> Result<Vec<PolicyRoleDto>, GovernanceError>;

    fn list_policies(&self) -> Result<Vec<ToolPolicyDto>, GovernanceError>;

    fn list_connection_policy_assignments(
        &self,
    ) -> Result<Vec<ConnectionPolicyAssignmentDto>, GovernanceError>;

    fn save_connection_policy_assignment(
        &self,
        assignment: ConnectionPolicyAssignmentDto,
    ) -> Result<ConnectionPolicyAssignmentDto, GovernanceError>;

    fn list_pending_executions(&self) -> Result<Vec<PendingExecutionSummary>, GovernanceError>;

    fn get_pending_execution(
        &self,
        pending_id: &str,
    ) -> Result<PendingExecutionDetail, GovernanceError>;

    fn approve_pending_execution(
        &self,
        pending_id: &str,
    ) -> Result<ApprovalOutcome, GovernanceError>;

    /// Rejects a pending execution. `reason` is returned to the requesting
    /// agent and recorded in the audit event.
    fn reject_pending_execution(
        &self,
        pending_id: &str,
        reason: Option<&str>,
    ) -> Result<ApprovalOutcome, GovernanceError>;
}

#[cfg(test)]
mod tests {
    use super::{ToolPolicy, ToolPolicyDto, validate_policy_class_sets};
    use crate::built_ins::builtin_policies;

    /// A policy with the given class ids, in the order they are written.
    fn policy(id: &str, allowed: &[&str], approval: &[&str]) -> ToolPolicyDto {
        let to_ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect();

        ToolPolicyDto {
            id: id.to_string(),
            allowed_tools: vec!["drop_table".to_string()],
            allowed_classes: to_ids(allowed),
            approval_classes: to_ids(approval),
        }
    }

    /// The three built-in policies keep parsing, which the parse check would
    /// otherwise stop: `builtin/read-only`, `builtin/write` and `builtin/admin`.
    #[test]
    fn every_builtin_policy_still_parses() {
        let ids: Vec<String> = builtin_policies()
            .into_iter()
            .map(|policy| policy.id)
            .collect();

        assert_eq!(
            ids,
            vec![
                "builtin/read-only".to_string(),
                "builtin/write".to_string(),
                "builtin/admin".to_string(),
            ]
        );

        for policy in builtin_policies() {
            let parsed = ToolPolicy::try_from(policy.clone());

            assert!(
                parsed.is_ok(),
                "{} must parse, got {:?}",
                policy.id,
                parsed.err()
            );
        }
    }

    /// The shape from #490: the top class is allowed while the two classes
    /// below it are only asked about. Parsing it must fail and name the pair.
    #[test]
    fn parsing_refuses_an_allowed_class_over_asked_classes_with_the_pair() {
        let dto = policy(
            "custom",
            &[
                "metadata",
                "read",
                "write",
                "destructive",
                "admin_destructive",
            ],
            &["admin_safe", "admin"],
        );

        let error = ToolPolicy::try_from(dto)
            .err()
            .expect("an allowed class over asked classes must be refused");

        for expected in ["custom", "AdminDestructive", "AdminSafe"] {
            assert!(error.contains(expected), "{error} must name {expected}");
        }
    }

    /// A gap in the middle of the allowed set leaves the classes above it
    /// allowed, so the policy cannot be stored.
    #[test]
    fn parsing_refuses_a_gap_in_the_middle_of_the_allowed_set() {
        let dto = policy("custom", &["metadata", "read", "admin"], &[]);

        let error = ToolPolicy::try_from(dto)
            .err()
            .expect("a gap in the allowed set must be refused");

        for expected in ["Admin", "Write"] {
            assert!(error.contains(expected), "{error} must name {expected}");
        }
    }

    /// Asking on a class above an allowed one is the stricter decision, so the
    /// policy is accepted.
    #[test]
    fn parsing_accepts_asking_above_an_allowed_class() {
        let dto = policy("custom", &["metadata", "read"], &["write"]);

        assert!(ToolPolicy::try_from(dto).is_ok());
    }

    /// The save path runs the same check, so a policy that the parse refuses is
    /// never written.
    #[test]
    fn saving_refuses_the_same_shape_and_keeps_a_coherent_one() {
        let incoherent = policy(
            "custom",
            &["metadata", "read", "write", "admin_destructive"],
            &["admin_safe", "admin"],
        );

        let error = validate_policy_class_sets(&incoherent)
            .expect_err("the save check must refuse the same shape as the parse check");

        assert!(error.contains("AdminSafe"), "{error}");
        assert!(error.contains("Destructive"), "{error}");

        assert_eq!(
            validate_policy_class_sets(&policy("custom", &["metadata", "read"], &["write"])),
            Ok(())
        );
    }

    /// The parse still reports an unknown class id before the class sets are
    /// read.
    #[test]
    fn parsing_still_reports_an_unknown_class_id() {
        let dto = policy("custom", &["metadata", "everything"], &[]);

        let error = ToolPolicy::try_from(dto)
            .err()
            .expect("an unknown class id must be refused");

        assert!(error.contains("everything"), "{error}");
    }
}
