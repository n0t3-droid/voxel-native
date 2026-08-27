//! Deterministic preview and admission contract for creator workflows.
//!
//! Editors and agents must execute the exact object revision that was reviewed.
//! The contract deliberately separates logical content, diagnostics and cost so
//! that changing any one of them invalidates an earlier preview receipt.

use std::fmt;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CreatorObjectId(String);

impl CreatorObjectId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        assert!(
            !value.trim().is_empty(),
            "creator object id must not be empty"
        );
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CreatorObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CreatorRevision(u64);

impl CreatorRevision {
    pub const INITIAL: Self = Self(1);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(&mut self) {
        self.0 = self.0.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentFingerprint(u64);

impl ContentFingerprint {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiagnosticFingerprint(u64);

impl DiagnosticFingerprint {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CostFingerprint(u64);

impl CostFingerprint {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

impl DiagnosticSeverity {
    const fn stable_tag(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CreatorDiagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

impl CreatorDiagnostic {
    pub fn new(
        code: impl Into<String>,
        severity: DiagnosticSeverity,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, DiagnosticSeverity::Error, message)
    }

    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, DiagnosticSeverity::Info, message)
    }

    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(code, DiagnosticSeverity::Warning, message)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CreatorCost {
    pub voxel_edits: u64,
    pub preview_cells: u64,
    pub estimated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreatorAdmissionLimits {
    pub max_voxel_edits: u64,
    pub max_preview_cells: u64,
    pub max_estimated_bytes: u64,
}

impl CreatorAdmissionLimits {
    pub const fn new(
        max_voxel_edits: u64,
        max_preview_cells: u64,
        max_estimated_bytes: u64,
    ) -> Self {
        Self {
            max_voxel_edits,
            max_preview_cells,
            max_estimated_bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatorPlanSnapshot {
    pub object_id: CreatorObjectId,
    pub revision: CreatorRevision,
    pub canonical_payload: Vec<u8>,
    pub cost: CreatorCost,
    pub diagnostics: Vec<CreatorDiagnostic>,
}

impl CreatorPlanSnapshot {
    pub fn new(
        object_id: CreatorObjectId,
        revision: CreatorRevision,
        canonical_payload: Vec<u8>,
        cost: CreatorCost,
        diagnostics: Vec<CreatorDiagnostic>,
    ) -> Self {
        Self {
            object_id,
            revision,
            canonical_payload,
            cost,
            diagnostics,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatorPlanEvaluation {
    pub object_id: CreatorObjectId,
    pub revision: CreatorRevision,
    pub content_fingerprint: ContentFingerprint,
    pub diagnostic_fingerprint: DiagnosticFingerprint,
    pub cost_fingerprint: CostFingerprint,
    pub cost: CreatorCost,
    pub diagnostics: Vec<CreatorDiagnostic>,
}

impl CreatorPlanEvaluation {
    pub fn is_admissible(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewReceipt {
    pub object_id: CreatorObjectId,
    pub revision: CreatorRevision,
    pub content_fingerprint: ContentFingerprint,
    pub diagnostic_fingerprint: DiagnosticFingerprint,
    pub cost_fingerprint: CostFingerprint,
    pub approved_cost: CreatorCost,
}

impl PreviewReceipt {
    pub fn binding_fingerprint(&self) -> u64 {
        let mut hasher = StableHasher::new("r93g.creator.preview_receipt.v1");
        hasher.write_bytes(self.object_id.as_str().as_bytes());
        hasher.write_u64(self.revision.get());
        hasher.write_u64(self.content_fingerprint.get());
        hasher.write_u64(self.diagnostic_fingerprint.get());
        hasher.write_u64(self.cost_fingerprint.get());
        hasher.write_u64(self.approved_cost.voxel_edits);
        hasher.write_u64(self.approved_cost.preview_cells);
        hasher.write_u64(self.approved_cost.estimated_bytes);
        hasher.finish()
    }

    pub fn short_code(&self) -> String {
        format!(
            "{:012X}",
            self.binding_fingerprint() & 0x0000_ffff_ffff_ffff
        )
    }

    pub fn matches(&self, evaluation: &CreatorPlanEvaluation) -> bool {
        authorize_commit(self, evaluation).is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitAuthorizationError {
    EvaluationRejected { error_count: usize },
    ObjectMismatch,
    RevisionMismatch,
    ContentMismatch,
    DiagnosticsMismatch,
    CostFingerprintMismatch,
    ApprovedCostMismatch,
}

impl fmt::Display for CommitAuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EvaluationRejected { error_count } => {
                write!(
                    formatter,
                    "creator evaluation contains {error_count} error(s)"
                )
            }
            Self::ObjectMismatch => formatter.write_str("creator object changed after preview"),
            Self::RevisionMismatch => formatter.write_str("creator revision changed after preview"),
            Self::ContentMismatch => formatter.write_str("creator content changed after preview"),
            Self::DiagnosticsMismatch => {
                formatter.write_str("creator diagnostics changed after preview")
            }
            Self::CostFingerprintMismatch => {
                formatter.write_str("creator cost estimate changed after preview")
            }
            Self::ApprovedCostMismatch => {
                formatter.write_str("creator execution cost differs from approved preview")
            }
        }
    }
}

impl std::error::Error for CommitAuthorizationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRejected {
    pub diagnostics: Vec<CreatorDiagnostic>,
}

impl PreviewRejected {
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .count()
    }
}

impl fmt::Display for PreviewRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "creator preview rejected with {} error(s)",
            self.error_count()
        )
    }
}

impl std::error::Error for PreviewRejected {}

pub fn evaluate_plan(
    snapshot: &CreatorPlanSnapshot,
    limits: CreatorAdmissionLimits,
) -> CreatorPlanEvaluation {
    let mut diagnostics = snapshot.diagnostics.clone();
    append_budget_diagnostics(&mut diagnostics, snapshot.cost, limits);
    sort_and_dedup_diagnostics(&mut diagnostics);

    CreatorPlanEvaluation {
        object_id: snapshot.object_id.clone(),
        revision: snapshot.revision,
        content_fingerprint: fingerprint_content(snapshot),
        diagnostic_fingerprint: fingerprint_diagnostics(&diagnostics),
        cost_fingerprint: fingerprint_cost(snapshot.cost),
        cost: snapshot.cost,
        diagnostics,
    }
}

pub fn issue_preview_receipt(
    evaluation: &CreatorPlanEvaluation,
) -> Result<PreviewReceipt, PreviewRejected> {
    if !evaluation.is_admissible() {
        return Err(PreviewRejected {
            diagnostics: evaluation.diagnostics.clone(),
        });
    }

    Ok(PreviewReceipt {
        object_id: evaluation.object_id.clone(),
        revision: evaluation.revision,
        content_fingerprint: evaluation.content_fingerprint,
        diagnostic_fingerprint: evaluation.diagnostic_fingerprint,
        cost_fingerprint: evaluation.cost_fingerprint,
        approved_cost: evaluation.cost,
    })
}

pub fn authorize_commit(
    receipt: &PreviewReceipt,
    evaluation: &CreatorPlanEvaluation,
) -> Result<(), CommitAuthorizationError> {
    if !evaluation.is_admissible() {
        return Err(CommitAuthorizationError::EvaluationRejected {
            error_count: evaluation.error_count(),
        });
    }
    if receipt.object_id != evaluation.object_id {
        return Err(CommitAuthorizationError::ObjectMismatch);
    }
    if receipt.revision != evaluation.revision {
        return Err(CommitAuthorizationError::RevisionMismatch);
    }
    if receipt.content_fingerprint != evaluation.content_fingerprint {
        return Err(CommitAuthorizationError::ContentMismatch);
    }
    if receipt.diagnostic_fingerprint != evaluation.diagnostic_fingerprint {
        return Err(CommitAuthorizationError::DiagnosticsMismatch);
    }
    if receipt.cost_fingerprint != evaluation.cost_fingerprint {
        return Err(CommitAuthorizationError::CostFingerprintMismatch);
    }
    if receipt.approved_cost != evaluation.cost {
        return Err(CommitAuthorizationError::ApprovedCostMismatch);
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CanonicalPayloadBuilder {
    bytes: Vec<u8>,
}

impl CanonicalPayloadBuilder {
    pub fn new(domain: &str) -> Self {
        let mut builder = Self { bytes: Vec::new() };
        builder.push_str(domain);
        builder
    }

    pub fn push_bool(&mut self, value: bool) -> &mut Self {
        self.bytes.push(u8::from(value));
        self
    }

    pub fn push_u16(&mut self, value: u16) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn push_u32(&mut self, value: u32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn push_u64(&mut self, value: u64) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn push_i32(&mut self, value: i32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub fn push_str(&mut self, value: &str) -> &mut Self {
        self.push_bytes(value.as_bytes())
    }

    pub fn push_bytes(&mut self, value: &[u8]) -> &mut Self {
        self.push_u64(value.len() as u64);
        self.bytes.extend_from_slice(value);
        self
    }

    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn append_budget_diagnostics(
    diagnostics: &mut Vec<CreatorDiagnostic>,
    cost: CreatorCost,
    limits: CreatorAdmissionLimits,
) {
    if cost.voxel_edits > limits.max_voxel_edits {
        diagnostics.push(CreatorDiagnostic::error(
            "budget.voxel_edits",
            format!(
                "{} voxel edits exceed the approved limit of {}",
                cost.voxel_edits, limits.max_voxel_edits
            ),
        ));
    }
    if cost.preview_cells > limits.max_preview_cells {
        diagnostics.push(CreatorDiagnostic::error(
            "budget.preview_cells",
            format!(
                "{} preview cells exceed the approved limit of {}",
                cost.preview_cells, limits.max_preview_cells
            ),
        ));
    }
    if cost.estimated_bytes > limits.max_estimated_bytes {
        diagnostics.push(CreatorDiagnostic::error(
            "budget.estimated_bytes",
            format!(
                "{} estimated bytes exceed the approved limit of {}",
                cost.estimated_bytes, limits.max_estimated_bytes
            ),
        ));
    }
}

fn sort_and_dedup_diagnostics(diagnostics: &mut Vec<CreatorDiagnostic>) {
    diagnostics.sort_by(|left, right| {
        left.severity
            .cmp(&right.severity)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    diagnostics.dedup();
}

fn fingerprint_content(snapshot: &CreatorPlanSnapshot) -> ContentFingerprint {
    let mut hasher = StableHasher::new("r93g.creator.content.v1");
    hasher.write_bytes(snapshot.object_id.as_str().as_bytes());
    hasher.write_bytes(&snapshot.canonical_payload);
    ContentFingerprint(hasher.finish())
}

fn fingerprint_diagnostics(diagnostics: &[CreatorDiagnostic]) -> DiagnosticFingerprint {
    let mut hasher = StableHasher::new("r93g.creator.diagnostics.v1");
    hasher.write_u64(diagnostics.len() as u64);
    for diagnostic in diagnostics {
        hasher.write_u8(diagnostic.severity.stable_tag());
        hasher.write_bytes(diagnostic.code.as_bytes());
        hasher.write_bytes(diagnostic.message.as_bytes());
    }
    DiagnosticFingerprint(hasher.finish())
}

fn fingerprint_cost(cost: CreatorCost) -> CostFingerprint {
    let mut hasher = StableHasher::new("r93g.creator.cost.v1");
    hasher.write_u64(cost.voxel_edits);
    hasher.write_u64(cost.preview_cells);
    hasher.write_u64(cost.estimated_bytes);
    CostFingerprint(hasher.finish())
}

struct StableHasher {
    state: u64,
}

impl StableHasher {
    fn new(domain: &str) -> Self {
        let mut hasher = Self {
            state: FNV_OFFSET_BASIS,
        };
        hasher.write_bytes(domain.as_bytes());
        hasher
    }

    fn write_u8(&mut self, value: u8) {
        self.write_raw(&[value]);
    }

    fn write_u64(&mut self, value: u64) {
        self.write_raw(&value.to_le_bytes());
    }

    fn write_bytes(&mut self, value: &[u8]) {
        self.write_u64(value.len() as u64);
        self.write_raw(value);
    }

    fn write_raw(&mut self, value: &[u8]) {
        for byte in value {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(FNV_PRIME);
        }
    }

    fn finish(self) -> u64 {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> CreatorAdmissionLimits {
        CreatorAdmissionLimits::new(100, 100, 1_000)
    }

    fn plan(payload: &[u8]) -> CreatorPlanSnapshot {
        CreatorPlanSnapshot::new(
            CreatorObjectId::new("test.house"),
            CreatorRevision::INITIAL,
            payload.to_vec(),
            CreatorCost {
                voxel_edits: 25,
                preview_cells: 25,
                estimated_bytes: 100,
            },
            Vec::new(),
        )
    }

    #[test]
    fn identical_plan_produces_identical_receipt() {
        let first = evaluate_plan(&plan(b"same"), limits());
        let second = evaluate_plan(&plan(b"same"), limits());

        assert_eq!(
            issue_preview_receipt(&first).unwrap(),
            issue_preview_receipt(&second).unwrap()
        );
    }

    #[test]
    fn content_change_invalidates_receipt_even_at_same_revision() {
        let original = evaluate_plan(&plan(b"first"), limits());
        let receipt = issue_preview_receipt(&original).unwrap();
        let changed = evaluate_plan(&plan(b"second"), limits());

        assert!(!receipt.matches(&changed));
    }

    #[test]
    fn revision_change_invalidates_receipt_even_with_same_content() {
        let original = evaluate_plan(&plan(b"same"), limits());
        let receipt = issue_preview_receipt(&original).unwrap();
        let mut revised = plan(b"same");
        revised.revision = CreatorRevision::new(2);

        assert!(!receipt.matches(&evaluate_plan(&revised, limits())));
    }

    #[test]
    fn diagnostics_are_order_independent_but_changes_invalidate_receipt() {
        let mut first = plan(b"same");
        first.diagnostics = vec![
            CreatorDiagnostic::warning("b", "second"),
            CreatorDiagnostic::warning("a", "first"),
        ];
        let mut reordered = first.clone();
        reordered.diagnostics.reverse();
        let first_evaluation = evaluate_plan(&first, limits());
        let reordered_evaluation = evaluate_plan(&reordered, limits());
        assert_eq!(
            first_evaluation.diagnostic_fingerprint,
            reordered_evaluation.diagnostic_fingerprint
        );

        let receipt = issue_preview_receipt(&first_evaluation).unwrap();
        reordered.diagnostics[0].message = "changed".to_owned();
        assert!(!receipt.matches(&evaluate_plan(&reordered, limits())));
    }

    #[test]
    fn duplicate_diagnostics_are_canonicalized_before_fingerprinting() {
        let duplicate = CreatorDiagnostic::warning("style.note", "Review trim");
        let mut duplicated = plan(b"same");
        duplicated.diagnostics = vec![duplicate.clone(), duplicate.clone()];
        let mut unique = plan(b"same");
        unique.diagnostics = vec![duplicate];

        let duplicated = evaluate_plan(&duplicated, limits());
        let unique = evaluate_plan(&unique, limits());

        assert_eq!(duplicated.diagnostics.len(), 1);
        assert_eq!(
            duplicated.diagnostic_fingerprint,
            unique.diagnostic_fingerprint
        );
    }

    #[test]
    fn warnings_are_reviewable_but_errors_are_not() {
        let mut warning_plan = plan(b"same");
        warning_plan
            .diagnostics
            .push(CreatorDiagnostic::warning("style.note", "Review trim"));
        let warning_evaluation = evaluate_plan(&warning_plan, limits());
        let receipt = issue_preview_receipt(&warning_evaluation).unwrap();
        assert_eq!(authorize_commit(&receipt, &warning_evaluation), Ok(()));

        warning_plan
            .diagnostics
            .push(CreatorDiagnostic::error("geometry.invalid", "Open volume"));
        assert!(issue_preview_receipt(&evaluate_plan(&warning_plan, limits())).is_err());
    }

    #[test]
    fn each_budget_dimension_is_enforced() {
        for cost in [
            CreatorCost {
                voxel_edits: 101,
                preview_cells: 1,
                estimated_bytes: 1,
            },
            CreatorCost {
                voxel_edits: 1,
                preview_cells: 101,
                estimated_bytes: 1,
            },
            CreatorCost {
                voxel_edits: 1,
                preview_cells: 1,
                estimated_bytes: 1_001,
            },
        ] {
            let mut over_budget = plan(b"same");
            over_budget.cost = cost;
            let evaluation = evaluate_plan(&over_budget, limits());
            assert!(!evaluation.is_admissible());
            assert_eq!(evaluation.error_count(), 1);
            assert!(issue_preview_receipt(&evaluation).is_err());
        }
    }

    #[test]
    fn exact_budget_limits_are_admissible() {
        let mut exact = plan(b"same");
        exact.cost = CreatorCost {
            voxel_edits: 100,
            preview_cells: 100,
            estimated_bytes: 1_000,
        };

        let evaluation = evaluate_plan(&exact, limits());
        assert!(evaluation.is_admissible());
        assert_eq!(evaluation.error_count(), 0);
        assert!(issue_preview_receipt(&evaluation).is_ok());
    }

    #[test]
    fn all_budget_failures_are_deterministic_and_reported_together() {
        let mut over_budget = plan(b"same");
        over_budget.cost = CreatorCost {
            voxel_edits: 101,
            preview_cells: 102,
            estimated_bytes: 1_003,
        };

        let first = evaluate_plan(&over_budget, limits());
        let second = evaluate_plan(&over_budget, limits());
        assert_eq!(first, second);
        assert_eq!(first.error_count(), 3);
        assert_eq!(
            first
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            vec![
                "budget.estimated_bytes",
                "budget.preview_cells",
                "budget.voxel_edits"
            ]
        );
    }

    #[test]
    fn receipt_binding_code_changes_with_revision_and_cost() {
        let original = evaluate_plan(&plan(b"same"), limits());
        let original_receipt = issue_preview_receipt(&original).unwrap();

        let mut revised = plan(b"same");
        revised.revision = CreatorRevision::new(2);
        let revised_receipt = issue_preview_receipt(&evaluate_plan(&revised, limits())).unwrap();

        let mut recosted = plan(b"same");
        recosted.cost.voxel_edits += 1;
        let recosted_receipt = issue_preview_receipt(&evaluate_plan(&recosted, limits())).unwrap();

        assert_ne!(original_receipt.short_code(), revised_receipt.short_code());
        assert_ne!(original_receipt.short_code(), recosted_receipt.short_code());
    }

    #[test]
    fn commit_authorization_reports_the_exact_invalidated_dimension() {
        let evaluation = evaluate_plan(&plan(b"same"), limits());
        let receipt = issue_preview_receipt(&evaluation).unwrap();

        let mut rejected = evaluation.clone();
        rejected
            .diagnostics
            .push(CreatorDiagnostic::error("geometry.invalid", "Open volume"));
        assert_eq!(
            authorize_commit(&receipt, &rejected),
            Err(CommitAuthorizationError::EvaluationRejected { error_count: 1 })
        );

        let mut changed = evaluation.clone();
        changed.object_id = CreatorObjectId::new("test.other");
        assert_eq!(
            authorize_commit(&receipt, &changed),
            Err(CommitAuthorizationError::ObjectMismatch)
        );

        let mut changed = evaluation.clone();
        changed.revision = CreatorRevision::new(2);
        assert_eq!(
            authorize_commit(&receipt, &changed),
            Err(CommitAuthorizationError::RevisionMismatch)
        );

        let mut changed = evaluation.clone();
        changed.content_fingerprint =
            ContentFingerprint(changed.content_fingerprint.get().wrapping_add(1));
        assert_eq!(
            authorize_commit(&receipt, &changed),
            Err(CommitAuthorizationError::ContentMismatch)
        );

        let mut changed = evaluation.clone();
        changed.diagnostic_fingerprint =
            DiagnosticFingerprint(changed.diagnostic_fingerprint.get().wrapping_add(1));
        assert_eq!(
            authorize_commit(&receipt, &changed),
            Err(CommitAuthorizationError::DiagnosticsMismatch)
        );

        let mut changed = evaluation.clone();
        changed.cost_fingerprint = CostFingerprint(changed.cost_fingerprint.get().wrapping_add(1));
        assert_eq!(
            authorize_commit(&receipt, &changed),
            Err(CommitAuthorizationError::CostFingerprintMismatch)
        );

        let mut changed = evaluation;
        changed.cost.voxel_edits += 1;
        assert_eq!(
            authorize_commit(&receipt, &changed),
            Err(CommitAuthorizationError::ApprovedCostMismatch)
        );
    }

    #[test]
    fn canonical_payload_builder_is_unambiguous() {
        let mut first = CanonicalPayloadBuilder::new("test");
        first.push_str("ab").push_str("c");
        let mut second = CanonicalPayloadBuilder::new("test");
        second.push_str("a").push_str("bc");

        assert_ne!(first.finish(), second.finish());
    }
}
