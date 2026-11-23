use serde::{Deserialize, Serialize};

/// Canonical governance classification used by policy and approvals.
///
/// # Two ladders, and what a ceiling means
///
/// The classes answer two questions that rank them differently, so this type
/// carries two orders:
///
/// - the **permission** ladder ([`permission_rank`](Self::permission_rank))
///   puts `AdminSafe` and `Admin` above `Destructive`: a grant to change
///   objects is not a grant to change rows, so a ceiling of `Destructive` does
///   not cover an object change;
/// - the **data-risk** ladder ([`data_risk_rank`](Self::data_risk_rank)) puts
///   `Destructive` above both admin classes: losing rows is worse than
///   changing an object.
///
/// Neither ladder implies the other, so a ceiling names the most of **each**:
/// an operation is within a ceiling only when it sits at or below the ceiling
/// on both. That is the question [`permits`](Self::permits) and
/// `datatab_core::ceiling_permits` answer, and it is the only way a ceiling is
/// read. Reading one on the permission ladder alone would let a confirmation
/// for an object change run a row removal nobody authorised (#487).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClassification {
    Metadata,
    Read,
    Write,
    Destructive,
    Admin,
    AdminSafe,
    AdminDestructive,
}

impl ExecutionClassification {
    /// Position on the permission ladder, from least to most restricted:
    /// `Metadata`, `Read`, `Write`, `Destructive`, `AdminSafe`, `Admin`,
    /// `AdminDestructive`. A higher rank is more restricted to permit; only
    /// `AdminDestructive` covers everything below it.
    ///
    /// The admin classes sit above `Destructive` here because a row-data
    /// permission and an object permission are separate grants this ladder
    /// does not merge: a ceiling of `Destructive` does not cover an `AdminSafe`
    /// change. This ladder alone does not answer whether an operation is
    /// within a ceiling — `AdminSafe` and `Admin` sit above `Destructive`
    /// here, so comparing on this ladder alone would admit a row removal under
    /// an object-change grant. A ceiling reads both ladders:
    /// [`permits`](Self::permits).
    ///
    /// An exhaustive `match` with no `_` arm, so adding a new
    /// [`ExecutionClassification`] variant fails the build instead of silently
    /// defaulting to a rank. The same holds for [`data_risk_rank`](Self::data_risk_rank).
    fn permission_rank(self) -> u8 {
        match self {
            Self::Metadata => 0,
            Self::Read => 1,
            Self::Write => 2,
            Self::Destructive => 3,
            Self::AdminSafe => 4,
            Self::Admin => 5,
            Self::AdminDestructive => 6,
        }
    }

    /// Position on the data-risk ladder, from least to most dangerous:
    /// `Metadata`, `Read`, `Write`, `AdminSafe`, `Admin`, `Destructive`,
    /// `AdminDestructive`.
    ///
    /// `Destructive` outranks `AdminSafe` and `Admin` here because losing data
    /// is worse than changing an object, so a destructive statement ranks above
    /// an object change. An exhaustive `match` with no `_` arm, like
    /// [`permission_rank`](Self::permission_rank).
    fn data_risk_rank(self) -> u8 {
        match self {
            Self::Metadata => 0,
            Self::Read => 1,
            Self::Write => 2,
            Self::AdminSafe => 3,
            Self::Admin => 4,
            Self::Destructive => 5,
            Self::AdminDestructive => 6,
        }
    }

    /// Returns the higher class on the permission ladder, i.e. of the two the
    /// one that is more restricted to permit.
    ///
    /// This is not the composition fold: `max` can return a class that one of
    /// its operands outranks on the data-risk ladder. Folding operands into one
    /// operation is [`compose`](Self::compose).
    pub fn max(self, other: Self) -> Self {
        if self.permission_rank() >= other.permission_rank() {
            self
        } else {
            other
        }
    }

    /// Whether confirming this class as a ceiling permits an operation that
    /// classifies as `operation`.
    ///
    /// A ceiling authorises the most of each ladder, so the operation must sit
    /// at or below the ceiling on **both**: on the permission ladder, because
    /// an object grant does not carry a wider data permission, and on the
    /// data-risk ladder, because an object grant does not carry a worse data
    /// risk. `AdminSafe.permits(Destructive)` is therefore false: a
    /// confirmation for an object change says nothing about removing rows.
    ///
    /// Equivalent to asking whether folding the operation into the ceiling
    /// would escalate it — `self.compose(operation) == self` — and the tests
    /// pin the two together for every pair.
    ///
    /// Every ceiling check in the app goes through this or through
    /// `datatab_core::ceiling_permits`, which delegates here; do not compare
    /// ranks at a call site.
    pub fn permits(self, operation: Self) -> bool {
        self.permission_rank() >= operation.permission_rank()
            && self.data_risk_rank() >= operation.data_risk_rank()
    }

    /// Composes two operand classes into the class of the operation they make
    /// together.
    ///
    /// The invariant is that the result is **never less restricted than either
    /// operand on either ladder**: a folded buffer must never be easier to
    /// permit than one of its own statements, or a policy could run the worse
    /// statement under a decision it granted for the milder one. The two
    /// ladders order `Destructive` against `AdminSafe` and `Admin` differently,
    /// so:
    ///
    /// - operands that are comparable on both ladders give the more restricted
    ///   of the two (`Write` with `Admin` gives `Admin`);
    /// - operands that are not — `Destructive` with `AdminSafe` or `Admin` —
    ///   give `AdminDestructive`, the one class at the top of both ladders.
    ///
    /// Use this, not [`max`](Self::max), whenever the classes of separate
    /// operands are folded into the single class of the operation they
    /// compose: a buffer of statements, or the operations of one `ALTER`.
    pub fn compose(self, other: Self) -> Self {
        let self_is_higher = self.permission_rank() >= other.permission_rank()
            && self.data_risk_rank() >= other.data_risk_rank();
        let other_is_higher = other.permission_rank() >= self.permission_rank()
            && other.data_risk_rank() >= self.data_risk_rank();

        if self_is_higher {
            self
        } else if other_is_higher {
            other
        } else {
            Self::AdminDestructive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ExecutionClassification;

    /// The escalation ladder from least to most restrictive. `max` must always
    /// return whichever of its two operands sits higher on this ladder; it is
    /// what names the permission level. A confirmed ceiling is read by
    /// [`ExecutionClassification::permits`] and a policy by exact set
    /// membership, never by `max`.
    const PERMISSION_ORDER: [ExecutionClassification; 7] = [
        ExecutionClassification::Metadata,
        ExecutionClassification::Read,
        ExecutionClassification::Write,
        ExecutionClassification::Destructive,
        ExecutionClassification::AdminSafe,
        ExecutionClassification::Admin,
        ExecutionClassification::AdminDestructive,
    ];

    /// The data-risk ladder from least to most dangerous. `compose` must never
    /// return a class below either operand on either ladder.
    const DATA_RISK_ORDER: [ExecutionClassification; 7] = [
        ExecutionClassification::Metadata,
        ExecutionClassification::Read,
        ExecutionClassification::Write,
        ExecutionClassification::AdminSafe,
        ExecutionClassification::Admin,
        ExecutionClassification::Destructive,
        ExecutionClassification::AdminDestructive,
    ];

    #[test]
    fn max_returns_higher_rank_for_every_pair() {
        for (lower_index, lower) in PERMISSION_ORDER.iter().enumerate() {
            for (higher_index, higher) in PERMISSION_ORDER.iter().enumerate() {
                let expected = if higher_index >= lower_index {
                    *higher
                } else {
                    *lower
                };

                assert_eq!(
                    lower.max(*higher),
                    expected,
                    "max({lower:?}, {higher:?}) must be the higher-ranked variant"
                );
                assert_eq!(
                    higher.max(*lower),
                    expected,
                    "max is symmetric: max({higher:?}, {lower:?}) must match"
                );
            }
        }
    }

    /// The invariant of #483: a folded operation is never less restricted than
    /// any of its operands, on either ladder. Reviewer 2's defect was a compose
    /// that returned `Destructive` for `Admin` with `Destructive`, and
    /// `Destructive` sits below `Admin` on the permission ladder, so an
    /// exact-set policy could run the worse statement under a decision it
    /// granted for the milder one.
    #[test]
    fn compose_is_never_less_restricted_than_an_operand() {
        for left in PERMISSION_ORDER {
            for right in PERMISSION_ORDER {
                let composed = left.compose(right);

                assert!(
                    composed.permission_rank() >= left.permission_rank()
                        && composed.permission_rank() >= right.permission_rank(),
                    "compose({left:?}, {right:?}) = {composed:?} sits below an operand on the permission ladder"
                );
                assert!(
                    composed.data_risk_rank() >= left.data_risk_rank()
                        && composed.data_risk_rank() >= right.data_risk_rank(),
                    "compose({left:?}, {right:?}) = {composed:?} sits below an operand on the data-risk ladder"
                );
            }
        }
    }

    /// `Destructive` with either admin class is the pair the two ladders order
    /// in opposite directions, so composition cannot pick one of them.
    #[test]
    fn compose_escalates_a_destructive_and_an_admin_operand_to_admin_destructive() {
        use ExecutionClassification::*;

        assert_eq!(AdminSafe.compose(Destructive), AdminDestructive);
        assert_eq!(Destructive.compose(AdminSafe), AdminDestructive);
        assert_eq!(Admin.compose(Destructive), AdminDestructive);
        assert_eq!(Destructive.compose(Admin), AdminDestructive);
    }

    /// Pairs the two ladders order the same way keep the more restricted one,
    /// so an ordinary script does not lose its class.
    #[test]
    fn compose_keeps_the_more_restricted_operand_for_comparable_pairs() {
        use ExecutionClassification::*;

        assert_eq!(Write.compose(Admin), Admin);
        assert_eq!(Write.compose(AdminSafe), AdminSafe);
        assert_eq!(Read.compose(AdminSafe), AdminSafe);
        assert_eq!(Destructive.compose(Write), Destructive);
        assert_eq!(Metadata.compose(Read), Read);
        assert_eq!(Admin.compose(AdminSafe), Admin);
        assert_eq!(AdminSafe.compose(Admin), Admin);
    }

    #[test]
    fn compose_keeps_admin_destructive_above_every_operand() {
        for other in DATA_RISK_ORDER {
            assert_eq!(
                ExecutionClassification::AdminDestructive.compose(other),
                ExecutionClassification::AdminDestructive,
                "AdminDestructive must outrank {other:?}"
            );
        }
    }

    /// The permission ladder keeps the admin classes above `Destructive`, as
    /// the stored policies expect: a ceiling of `Destructive` does not cover an
    /// `AdminSafe` change. A ceiling reads the data-risk ladder as well, which
    /// is the other direction: `AdminSafe` and `Admin` do not permit
    /// `Destructive`.
    #[test]
    fn permission_ladder_keeps_admin_safe_above_destructive() {
        use ExecutionClassification::*;

        assert_eq!(AdminSafe.max(Destructive), AdminSafe);
        assert_eq!(Destructive.max(AdminSafe), AdminSafe);
    }

    /// `permits(ceiling, operation)` for every pair, one row per ceiling in
    /// [`PERMISSION_ORDER`] order and one column per operation in the same
    /// order.
    ///
    /// The rule: an operation is within a ceiling only at or below it on
    /// **both** ladders. The answers are written out as literals rather than
    /// derived from a rank, so changing either ladder has to edit a cell here
    /// and cannot drift past this table. The two `false` cells that the
    /// permission ladder alone would answer `true` are `AdminSafe` and `Admin`
    /// against `Destructive`: an object-change ceiling does not admit a row
    /// removal (#487).
    const PERMITS: [[bool; 7]; 7] = [
        /* Metadata        */ [true, false, false, false, false, false, false],
        /* Read            */ [true, true, false, false, false, false, false],
        /* Write           */ [true, true, true, false, false, false, false],
        /* Destructive     */ [true, true, true, true, false, false, false],
        /* AdminSafe       */ [true, true, true, false, true, false, false],
        /* Admin           */ [true, true, true, false, true, true, false],
        /* AdminDestructive*/ [true, true, true, true, true, true, true],
    ];

    #[test]
    fn permits_pair_table_matches_every_class_against_every_class() {
        for (ceiling_index, ceiling) in PERMISSION_ORDER.iter().enumerate() {
            for (operation_index, operation) in PERMISSION_ORDER.iter().enumerate() {
                assert_eq!(
                    ceiling.permits(*operation),
                    PERMITS[ceiling_index][operation_index],
                    "permits({ceiling:?}, {operation:?})"
                );
            }
        }
    }

    /// The decision #487 flips: an `AdminSafe` or `Admin` ceiling used to
    /// admit `Destructive`, because the permission ladder alone ranks both
    /// admin classes above it. The old answer was unsafe — an authorisation
    /// that names an object change let a row removal run with no further
    /// approval.
    #[test]
    fn an_object_change_ceiling_refuses_a_row_removal() {
        use ExecutionClassification::*;

        // The old answer, from the permission ladder alone: both ceilings sit
        // above `Destructive`, so a permission comparison admits it.
        assert_eq!(AdminSafe.max(Destructive), AdminSafe);
        assert_eq!(Admin.max(Destructive), Admin);

        // The answer this issue makes true.
        assert!(!AdminSafe.permits(Destructive));
        assert!(!Admin.permits(Destructive));
        assert!(AdminDestructive.permits(Destructive));
        assert!(Destructive.permits(Destructive));
    }

    /// `permits` and composition read the same two ladders, so a ceiling that
    /// admits an operation is exactly a ceiling that folding the operation
    /// into it cannot escalate. Pinned for every pair, so the two can never
    /// drift apart.
    #[test]
    fn permits_agrees_with_composing_the_operation_into_the_ceiling() {
        for ceiling in PERMISSION_ORDER {
            for operation in PERMISSION_ORDER {
                assert_eq!(
                    ceiling.permits(operation),
                    ceiling.compose(operation) == ceiling,
                    "ceiling {ceiling:?} against operation {operation:?}"
                );
            }
        }
    }

    #[test]
    fn max_and_compose_are_idempotent_for_each_variant() {
        for variant in PERMISSION_ORDER {
            assert_eq!(variant.max(variant), variant);
            assert_eq!(variant.compose(variant), variant);
        }
    }

    #[test]
    fn serde_round_trip_preserves_every_variant() {
        for variant in PERMISSION_ORDER {
            let json = serde_json::to_string(&variant).expect("serialize");
            let decoded: ExecutionClassification =
                serde_json::from_str(&json).expect("deserialize");

            assert_eq!(decoded, variant, "round-trip must preserve {variant:?}");
        }
    }

    #[test]
    fn serde_uses_snake_case_wire_names() {
        let cases = [
            (ExecutionClassification::Metadata, "\"metadata\""),
            (ExecutionClassification::Read, "\"read\""),
            (ExecutionClassification::Write, "\"write\""),
            (ExecutionClassification::Destructive, "\"destructive\""),
            (ExecutionClassification::AdminSafe, "\"admin_safe\""),
            (ExecutionClassification::Admin, "\"admin\""),
            (
                ExecutionClassification::AdminDestructive,
                "\"admin_destructive\"",
            ),
        ];

        for (variant, wire) in cases {
            assert_eq!(serde_json::to_string(&variant).expect("serialize"), wire);
        }
    }
}
