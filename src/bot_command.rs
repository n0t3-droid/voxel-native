//! Deterministic command planning and lifecycle management for builder bots.
//!
//! This module intentionally contains no systems or world access. It is a
//! resource-ready state machine that can be driven by UI, networking, or bot
//! execution systems without making command validation depend on frame timing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use bevy::prelude::{IVec3, Resource};

use crate::creator_contract::{
    authorize_commit, evaluate_plan, issue_preview_receipt, CanonicalPayloadBuilder,
    CommitAuthorizationError, CreatorAdmissionLimits, CreatorCost, CreatorDiagnostic,
    CreatorObjectId, CreatorPlanEvaluation, CreatorPlanSnapshot, CreatorRevision,
    DiagnosticSeverity, PreviewReceipt,
};

/// Stable identifier for a bot known to the command dispatcher.
pub type BotId = u64;

/// Stable identifier for a named or persisted bot group.
pub type GroupId = u64;

/// A command identifier allocated deterministically by [`BotCommandStateMachine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(u64);

impl CommandId {
    pub const fn from_raw(value: u64) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Bots that should receive a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRecipients {
    All,
    Selected(Vec<BotId>),
    Group(GroupId),
}

/// The operation bots should perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOperation {
    Inspect,
    Pencil,
    Rectangle,
    PushPull,
    Room,
    CutOpening,
    Move,
    Rotate,
    Scale,
    Paint,
    Road,
    ClearFlatten,
    Repair,
}

impl CommandOperation {
    /// Relative voxel work per target voxel.
    ///
    /// Read-only inspection has no voxel mutation cost. Transform and
    /// construction operations account for additional read/write or generated
    /// voxel work while remaining deterministic and integer-only.
    pub const fn voxel_cost_factor(self) -> u64 {
        match self {
            Self::Inspect => 0,
            Self::Pencil | Self::Rectangle | Self::CutOpening | Self::Paint => 1,
            Self::PushPull | Self::Move | Self::Rotate | Self::ClearFlatten | Self::Repair => 2,
            Self::Scale | Self::Road => 3,
            Self::Room => 4,
        }
    }

    const fn stable_tag(self) -> u16 {
        match self {
            Self::Inspect => 0,
            Self::Pencil => 1,
            Self::Rectangle => 2,
            Self::PushPull => 3,
            Self::Room => 4,
            Self::CutOpening => 5,
            Self::Move => 6,
            Self::Rotate => 7,
            Self::Scale => 8,
            Self::Paint => 9,
            Self::Road => 10,
            Self::ClearFlatten => 11,
            Self::Repair => 12,
        }
    }
}

/// Exact integer-space geometry addressed by a command.
///
/// `Area` bounds are inclusive. `Path` preserves waypoint order. `Selection`
/// preserves the selected voxel list; duplicate entries are reported during
/// preview and counted once for cost purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandTarget {
    Point(IVec3),
    Area { min: IVec3, max: IVec3 },
    Path(Vec<IVec3>),
    Selection(Vec<IVec3>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Point,
    Area,
    Path,
    Selection,
}

impl TargetKind {
    const fn stable_tag(self) -> u16 {
        match self {
            Self::Point => 0,
            Self::Area => 1,
            Self::Path => 2,
            Self::Selection => 3,
        }
    }
}

impl CommandTarget {
    pub const fn kind(&self) -> TargetKind {
        match self {
            Self::Point(_) => TargetKind::Point,
            Self::Area { .. } => TargetKind::Area,
            Self::Path(_) => TargetKind::Path,
            Self::Selection(_) => TargetKind::Selection,
        }
    }
}

/// Public lifecycle states for one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandState {
    Draft,
    PreviewReady,
    Approved,
    Running,
    Paused,
    Completed,
    Cancelled,
    Blocked,
}

impl CommandState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSeverity {
    Warning,
    Error,
}

/// Deterministic preview findings. Error issues prevent approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewIssue {
    EmptySelectedRecipients,
    DuplicateSelectedRecipient(BotId),
    InvalidArea { min: IVec3, max: IVec3 },
    EmptyPath,
    SinglePointPath,
    EmptySelection,
    DuplicateSelectionVoxel(IVec3),
    VoxelCostSaturated,
    ChunkCostSaturated,
    VoxelLimitExceeded { estimated: u64, limit: u64 },
    ChunkLimitExceeded { estimated: u64, limit: u64 },
}

impl PreviewIssue {
    pub const fn severity(&self) -> IssueSeverity {
        match self {
            Self::EmptySelectedRecipients
            | Self::InvalidArea { .. }
            | Self::EmptyPath
            | Self::EmptySelection
            | Self::DuplicateSelectedRecipient(_)
            | Self::VoxelCostSaturated
            | Self::ChunkCostSaturated
            | Self::VoxelLimitExceeded { .. }
            | Self::ChunkLimitExceeded { .. } => IssueSeverity::Error,
            Self::SinglePointPath | Self::DuplicateSelectionVoxel(_) => IssueSeverity::Warning,
        }
    }
}

/// Integer-only cost estimate produced before approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPreview {
    pub estimated_voxel_cost: u64,
    pub estimated_chunk_cost: u64,
    pub issues: Vec<PreviewIssue>,
}

impl CommandPreview {
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity() == IssueSeverity::Error)
    }
}

/// Deterministic preview configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewLimits {
    pub chunk_edge: u32,
    pub max_voxel_cost: u64,
    pub max_chunk_cost: u64,
}

impl PreviewLimits {
    pub const fn new(chunk_edge: u32, max_voxel_cost: u64, max_chunk_cost: u64) -> Self {
        Self {
            chunk_edge,
            max_voxel_cost,
            max_chunk_cost,
        }
    }
}

impl Default for PreviewLimits {
    fn default() -> Self {
        Self::new(16, 1_000_000, 4_096)
    }
}

/// An immutable view of the data frozen at approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedCommand {
    pub operation: CommandOperation,
    pub target: CommandTarget,
    pub recipients: CommandRecipients,
    pub preview: CommandPreview,
    pub limits: PreviewLimits,
    pub receipt: PreviewReceipt,
}

/// Stable identity of the exact approved revision claimed by the world executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DispatchKey {
    pub command_id: CommandId,
    pub revision: CreatorRevision,
    pub approval_fingerprint: u64,
}

/// One-shot capability handed to the world executor.
///
/// This type deliberately does not implement `Clone` or `Copy`: the state
/// machine issues it once, and the executor remains its single owner until
/// completion succeeds or the job is retired.
#[derive(Debug)]
pub struct DispatchPermit {
    key: DispatchKey,
    approved: ApprovedCommand,
}

impl DispatchPermit {
    pub const fn key(&self) -> DispatchKey {
        self.key
    }

    pub const fn approved(&self) -> &ApprovedCommand {
        &self.approved
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompletionSummary {
    pub applied_voxel_edits: u64,
    pub touched_chunks: u64,
    pub spawned_projects: u32,
}

/// One command and its guarded lifecycle data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotCommand {
    id: CommandId,
    revision: CreatorRevision,
    operation: CommandOperation,
    target: CommandTarget,
    recipients: CommandRecipients,
    state: CommandState,
    preview: Option<CommandPreview>,
    approved: Option<ApprovedCommand>,
    dispatch_key: Option<DispatchKey>,
    completion: Option<CompletionSummary>,
    blocked_from: Option<CommandState>,
    block_reason: Option<String>,
}

impl BotCommand {
    pub const fn id(&self) -> CommandId {
        self.id
    }

    pub const fn operation(&self) -> CommandOperation {
        self.operation
    }

    pub const fn revision(&self) -> CreatorRevision {
        self.revision
    }

    pub const fn state(&self) -> CommandState {
        self.state
    }

    pub const fn target(&self) -> &CommandTarget {
        &self.target
    }

    pub const fn recipients(&self) -> &CommandRecipients {
        &self.recipients
    }

    pub const fn preview(&self) -> Option<&CommandPreview> {
        self.preview.as_ref()
    }

    pub const fn approved(&self) -> Option<&ApprovedCommand> {
        self.approved.as_ref()
    }

    pub const fn dispatch_key(&self) -> Option<DispatchKey> {
        self.dispatch_key
    }

    pub const fn completion(&self) -> Option<CompletionSummary> {
        self.completion
    }

    pub fn block_reason(&self) -> Option<&str> {
        self.block_reason.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    UnknownCommand(CommandId),
    IdSpaceExhausted,
    InvalidPreviewConfiguration {
        chunk_edge: u32,
    },
    InvalidTransition {
        id: CommandId,
        from: CommandState,
        to: CommandState,
    },
    ExecuteBeforeApproval(CommandId),
    CommandFrozen(CommandId),
    PreviewHasErrors(CommandId),
    MissingPreview(CommandId),
    MissingBlockReason(CommandId),
    MissingBlockedState(CommandId),
    AuthorizationFailed {
        id: CommandId,
        reason: CommitAuthorizationError,
    },
    DispatchKeyMismatch {
        id: CommandId,
        expected: Option<DispatchKey>,
        actual: DispatchKey,
    },
    CompletionCostExceeded {
        id: CommandId,
        applied_voxel_edits: u64,
        approved_voxel_cost: u64,
        touched_chunks: u64,
        approved_chunk_cost: u64,
    },
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(id) => write!(formatter, "unknown command {id}"),
            Self::IdSpaceExhausted => formatter.write_str("command id space exhausted"),
            Self::InvalidPreviewConfiguration { chunk_edge } => {
                write!(
                    formatter,
                    "preview chunk edge must be non-zero, got {chunk_edge}"
                )
            }
            Self::InvalidTransition { id, from, to } => {
                write!(
                    formatter,
                    "command {id} cannot transition from {from:?} to {to:?}"
                )
            }
            Self::ExecuteBeforeApproval(id) => {
                write!(formatter, "command {id} cannot execute before approval")
            }
            Self::CommandFrozen(id) => {
                write!(formatter, "approved command {id} is frozen")
            }
            Self::PreviewHasErrors(id) => {
                write!(
                    formatter,
                    "command {id} preview contains approval-blocking errors"
                )
            }
            Self::MissingPreview(id) => write!(formatter, "command {id} has no preview"),
            Self::MissingBlockReason(id) => {
                write!(formatter, "command {id} requires a non-empty block reason")
            }
            Self::MissingBlockedState(id) => {
                write!(
                    formatter,
                    "command {id} has no state to restore after unblock"
                )
            }
            Self::AuthorizationFailed { id, reason } => {
                write!(
                    formatter,
                    "command {id} no longer matches its approved preview: {reason}"
                )
            }
            Self::DispatchKeyMismatch {
                id,
                expected,
                actual,
            } => {
                write!(
                    formatter,
                    "command {id} dispatch key mismatch: expected {expected:?}, got {actual:?}"
                )
            }
            Self::CompletionCostExceeded {
                id,
                applied_voxel_edits,
                approved_voxel_cost,
                touched_chunks,
                approved_chunk_cost,
            } => {
                write!(
                    formatter,
                    "command {id} executor exceeded its approved cost: \
                     {applied_voxel_edits}/{approved_voxel_cost} voxel edits, \
                     {touched_chunks}/{approved_chunk_cost} chunks"
                )
            }
        }
    }
}

impl std::error::Error for CommandError {}

/// Bevy resource that owns commands and enforces all lifecycle transitions.
///
/// IDs begin at one and increase monotonically. A `BTreeMap` gives stable
/// iteration order, so the same sequence of inputs has the same IDs, previews,
/// issue order, and command order on every run.
#[derive(Resource, Debug)]
pub struct BotCommandStateMachine {
    commands: BTreeMap<CommandId, BotCommand>,
    next_id: u64,
    limits: PreviewLimits,
}

impl Default for BotCommandStateMachine {
    fn default() -> Self {
        Self::new(PreviewLimits::default())
            .expect("default bot command preview limits must be valid")
    }
}

impl BotCommandStateMachine {
    pub fn new(limits: PreviewLimits) -> Result<Self, CommandError> {
        if limits.chunk_edge == 0 {
            return Err(CommandError::InvalidPreviewConfiguration {
                chunk_edge: limits.chunk_edge,
            });
        }

        Ok(Self {
            commands: BTreeMap::new(),
            next_id: 1,
            limits,
        })
    }

    pub const fn limits(&self) -> PreviewLimits {
        self.limits
    }

    pub fn commands(&self) -> impl ExactSizeIterator<Item = &BotCommand> {
        self.commands.values()
    }

    pub fn command(&self, id: CommandId) -> Result<&BotCommand, CommandError> {
        self.commands
            .get(&id)
            .ok_or(CommandError::UnknownCommand(id))
    }

    pub fn create(
        &mut self,
        operation: CommandOperation,
        target: CommandTarget,
        recipients: CommandRecipients,
    ) -> Result<CommandId, CommandError> {
        let id = CommandId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(CommandError::IdSpaceExhausted)?;

        self.commands.insert(
            id,
            BotCommand {
                id,
                revision: CreatorRevision::INITIAL,
                operation,
                target,
                recipients,
                state: CommandState::Draft,
                preview: None,
                approved: None,
                dispatch_key: None,
                completion: None,
                blocked_from: None,
                block_reason: None,
            },
        );
        Ok(id)
    }

    pub fn set_operation(
        &mut self,
        id: CommandId,
        operation: CommandOperation,
    ) -> Result<(), CommandError> {
        let command = self.editable_command(id)?;
        if command.operation == operation {
            return Ok(());
        }
        command.operation = operation;
        mark_command_edited(command);
        Ok(())
    }

    pub fn set_target(&mut self, id: CommandId, target: CommandTarget) -> Result<(), CommandError> {
        let command = self.editable_command(id)?;
        if command.target == target {
            return Ok(());
        }
        command.target = target;
        mark_command_edited(command);
        Ok(())
    }

    pub fn set_recipients(
        &mut self,
        id: CommandId,
        recipients: CommandRecipients,
    ) -> Result<(), CommandError> {
        let command = self.editable_command(id)?;
        if command.recipients == recipients {
            return Ok(());
        }
        command.recipients = recipients;
        mark_command_edited(command);
        Ok(())
    }

    pub fn prepare_preview(&mut self, id: CommandId) -> Result<&CommandPreview, CommandError> {
        let limits = self.limits;
        let command = self.command_mut(id)?;
        ensure_transition(command, CommandState::PreviewReady)?;

        let operation = command.operation();
        let preview = estimate_preview(operation, &command.target, &command.recipients, limits);
        command.preview = Some(preview);
        command.state = CommandState::PreviewReady;
        Ok(command
            .preview
            .as_ref()
            .expect("preview was assigned immediately above"))
    }

    pub fn approve(&mut self, id: CommandId) -> Result<&ApprovedCommand, CommandError> {
        let limits = self.limits;
        let command = self.command_mut(id)?;
        ensure_transition(command, CommandState::Approved)?;

        let preview = command
            .preview
            .as_ref()
            .ok_or(CommandError::MissingPreview(id))?;
        if preview.has_errors() {
            return Err(CommandError::PreviewHasErrors(id));
        }

        let operation = command.operation();
        let evaluation = evaluate_command_plan(
            id,
            command.revision,
            operation,
            &command.target,
            &command.recipients,
            preview,
            limits,
        );
        let receipt =
            issue_preview_receipt(&evaluation).map_err(|_| CommandError::PreviewHasErrors(id))?;

        command.approved = Some(ApprovedCommand {
            operation,
            target: command.target.clone(),
            recipients: command.recipients.clone(),
            preview: preview.clone(),
            limits,
            receipt,
        });
        command.state = CommandState::Approved;
        Ok(command
            .approved
            .as_ref()
            .expect("approved snapshot was assigned immediately above"))
    }

    /// Authorize an approved command for the world executor.
    ///
    /// This transition does not itself mutate the world. The executor must
    /// subsequently claim the one-shot dispatch permit.
    pub fn request_execution(&mut self, id: CommandId) -> Result<(), CommandError> {
        let command = self.command_mut(id)?;
        {
            let approved = command
                .approved
                .as_ref()
                .ok_or(CommandError::ExecuteBeforeApproval(id))?;
            let approved_evaluation = evaluate_command_plan(
                id,
                approved.receipt.revision,
                approved.operation,
                &approved.target,
                &approved.recipients,
                &approved.preview,
                approved.limits,
            );
            authorize_command(id, &approved.receipt, &approved_evaluation)?;

            let current_preview = command
                .preview
                .as_ref()
                .ok_or(CommandError::MissingPreview(id))?;
            let operation = command.operation();
            let current_evaluation = evaluate_command_plan(
                id,
                command.revision,
                operation,
                &command.target,
                &command.recipients,
                current_preview,
                approved.limits,
            );
            authorize_command(id, &approved.receipt, &current_evaluation)?;
        }
        transition(command, CommandState::Running)
    }

    /// Backward-compatible alias for callers that already use `execute`.
    pub fn execute(&mut self, id: CommandId) -> Result<(), CommandError> {
        self.request_execution(id)
    }

    /// Claim the exact approved revision once.
    ///
    /// `Ok(None)` means the command is not currently runnable or has already
    /// been claimed. A pause/block cycle retains the original claim.
    pub fn claim_dispatch(
        &mut self,
        id: CommandId,
    ) -> Result<Option<DispatchPermit>, CommandError> {
        let command = self.command_mut(id)?;
        if command.state != CommandState::Running || command.dispatch_key.is_some() {
            return Ok(None);
        }

        let approved = command
            .approved
            .as_ref()
            .ok_or(CommandError::ExecuteBeforeApproval(id))?;
        let evaluation = evaluate_command_plan(
            id,
            command.revision,
            command.operation,
            &command.target,
            &command.recipients,
            command
                .preview
                .as_ref()
                .ok_or(CommandError::MissingPreview(id))?,
            approved.limits,
        );
        authorize_command(id, &approved.receipt, &evaluation)?;

        let key = DispatchKey {
            command_id: id,
            revision: approved.receipt.revision,
            approval_fingerprint: approved.receipt.binding_fingerprint(),
        };
        command.dispatch_key = Some(key);
        Ok(Some(DispatchPermit {
            key,
            approved: approved.clone(),
        }))
    }

    pub fn pause(&mut self, id: CommandId) -> Result<(), CommandError> {
        let command = self.command_mut(id)?;
        transition(command, CommandState::Paused)
    }

    pub fn resume(&mut self, id: CommandId) -> Result<(), CommandError> {
        let command = self.command_mut(id)?;
        transition(command, CommandState::Running)
    }

    /// Complete a command only from the one-shot permit issued to the executor.
    pub fn complete_dispatch(
        &mut self,
        permit: &DispatchPermit,
        summary: CompletionSummary,
    ) -> Result<(), CommandError> {
        let id = permit.key.command_id;
        let command = self.command_mut(id)?;
        if command.dispatch_key != Some(permit.key) {
            return Err(CommandError::DispatchKeyMismatch {
                id,
                expected: command.dispatch_key,
                actual: permit.key,
            });
        }
        if command.approved.as_ref() != Some(&permit.approved) {
            return Err(CommandError::DispatchKeyMismatch {
                id,
                expected: command.dispatch_key,
                actual: permit.key,
            });
        }
        let approved = command
            .approved
            .as_ref()
            .ok_or(CommandError::ExecuteBeforeApproval(id))?;
        if summary.applied_voxel_edits > approved.preview.estimated_voxel_cost
            || summary.touched_chunks > approved.preview.estimated_chunk_cost
        {
            return Err(CommandError::CompletionCostExceeded {
                id,
                applied_voxel_edits: summary.applied_voxel_edits,
                approved_voxel_cost: approved.preview.estimated_voxel_cost,
                touched_chunks: summary.touched_chunks,
                approved_chunk_cost: approved.preview.estimated_chunk_cost,
            });
        }
        transition(command, CommandState::Completed)?;
        command.completion = Some(summary);
        Ok(())
    }

    pub fn cancel(&mut self, id: CommandId) -> Result<(), CommandError> {
        let command = self.command_mut(id)?;
        transition(command, CommandState::Cancelled)?;
        command.blocked_from = None;
        command.block_reason = None;
        Ok(())
    }

    /// Temporarily block an approved or active command.
    ///
    /// `unblock` restores the precise state that was interrupted. Target and
    /// recipients remain frozen throughout this cycle.
    pub fn block(&mut self, id: CommandId, reason: impl Into<String>) -> Result<(), CommandError> {
        let reason = reason.into();
        let command = self.command_mut(id)?;
        if reason.trim().is_empty() {
            return Err(CommandError::MissingBlockReason(id));
        }

        ensure_transition(command, CommandState::Blocked)?;
        command.blocked_from = Some(command.state);
        command.block_reason = Some(reason);
        command.state = CommandState::Blocked;
        Ok(())
    }

    pub fn unblock(&mut self, id: CommandId) -> Result<(), CommandError> {
        let command = self.command_mut(id)?;
        let restore = command
            .blocked_from
            .ok_or(CommandError::MissingBlockedState(id))?;
        ensure_transition(command, restore)?;
        command.state = restore;
        command.blocked_from = None;
        command.block_reason = None;
        Ok(())
    }

    fn command_mut(&mut self, id: CommandId) -> Result<&mut BotCommand, CommandError> {
        self.commands
            .get_mut(&id)
            .ok_or(CommandError::UnknownCommand(id))
    }

    fn editable_command(&mut self, id: CommandId) -> Result<&mut BotCommand, CommandError> {
        let command = self.command_mut(id)?;
        if command.approved.is_some() {
            return Err(CommandError::CommandFrozen(id));
        }
        if !matches!(
            command.state,
            CommandState::Draft | CommandState::PreviewReady
        ) {
            return Err(CommandError::InvalidTransition {
                id,
                from: command.state,
                to: CommandState::Draft,
            });
        }
        Ok(command)
    }
}

fn mark_command_edited(command: &mut BotCommand) {
    command.revision.next();
    command.preview = None;
    command.dispatch_key = None;
    command.completion = None;
    command.state = CommandState::Draft;
}

fn transition(command: &mut BotCommand, to: CommandState) -> Result<(), CommandError> {
    ensure_transition(command, to)?;
    command.state = to;
    Ok(())
}

fn ensure_transition(command: &BotCommand, to: CommandState) -> Result<(), CommandError> {
    let valid = matches!(
        (command.state, to),
        (CommandState::Draft, CommandState::PreviewReady)
            | (CommandState::Draft, CommandState::Cancelled)
            | (CommandState::PreviewReady, CommandState::PreviewReady)
            | (CommandState::PreviewReady, CommandState::Approved)
            | (CommandState::PreviewReady, CommandState::Cancelled)
            | (CommandState::Approved, CommandState::Running)
            | (CommandState::Approved, CommandState::Cancelled)
            | (CommandState::Approved, CommandState::Blocked)
            | (CommandState::Running, CommandState::Paused)
            | (CommandState::Running, CommandState::Completed)
            | (CommandState::Running, CommandState::Cancelled)
            | (CommandState::Running, CommandState::Blocked)
            | (CommandState::Paused, CommandState::Running)
            | (CommandState::Paused, CommandState::Cancelled)
            | (CommandState::Paused, CommandState::Blocked)
            | (CommandState::Blocked, CommandState::Approved)
            | (CommandState::Blocked, CommandState::Running)
            | (CommandState::Blocked, CommandState::Paused)
            | (CommandState::Blocked, CommandState::Cancelled)
    );

    if valid {
        Ok(())
    } else {
        Err(CommandError::InvalidTransition {
            id: command.id,
            from: command.state,
            to,
        })
    }
}

fn authorize_command(
    id: CommandId,
    receipt: &PreviewReceipt,
    evaluation: &CreatorPlanEvaluation,
) -> Result<(), CommandError> {
    authorize_commit(receipt, evaluation)
        .map_err(|reason| CommandError::AuthorizationFailed { id, reason })
}

fn evaluate_command_plan(
    id: CommandId,
    revision: CreatorRevision,
    operation: CommandOperation,
    target: &CommandTarget,
    recipients: &CommandRecipients,
    preview: &CommandPreview,
    limits: PreviewLimits,
) -> CreatorPlanEvaluation {
    let snapshot = CreatorPlanSnapshot::new(
        CreatorObjectId::new(format!("bot-command/{id}")),
        revision,
        command_payload(operation, target, recipients, limits),
        CreatorCost {
            voxel_edits: preview.estimated_voxel_cost,
            preview_cells: preview.estimated_chunk_cost,
            estimated_bytes: preview
                .estimated_voxel_cost
                .saturating_mul(2)
                .saturating_add(preview.estimated_chunk_cost.saturating_mul(64)),
        },
        preview
            .issues
            .iter()
            .map(preview_issue_diagnostic)
            .collect(),
    );

    let max_estimated_bytes = limits
        .max_voxel_cost
        .saturating_mul(2)
        .saturating_add(limits.max_chunk_cost.saturating_mul(64));
    evaluate_plan(
        &snapshot,
        CreatorAdmissionLimits::new(
            limits.max_voxel_cost,
            limits.max_chunk_cost,
            max_estimated_bytes,
        ),
    )
}

fn command_payload(
    operation: CommandOperation,
    target: &CommandTarget,
    recipients: &CommandRecipients,
    limits: PreviewLimits,
) -> Vec<u8> {
    let mut payload = CanonicalPayloadBuilder::new("r93g.bot_command.v1");
    payload.push_u16(operation.stable_tag());
    push_target_payload(&mut payload, target);
    push_recipient_payload(&mut payload, recipients);
    payload
        .push_u32(limits.chunk_edge)
        .push_u64(limits.max_voxel_cost)
        .push_u64(limits.max_chunk_cost);
    payload.finish()
}

fn push_target_payload(payload: &mut CanonicalPayloadBuilder, target: &CommandTarget) {
    payload.push_u16(target.kind().stable_tag());
    match target {
        CommandTarget::Point(point) => {
            push_point_payload(payload, *point);
        }
        CommandTarget::Area { min, max } => {
            push_point_payload(payload, *min);
            push_point_payload(payload, *max);
        }
        CommandTarget::Path(points) => {
            payload.push_u64(usize_to_u64(points.len()));
            for &point in points {
                push_point_payload(payload, point);
            }
        }
        CommandTarget::Selection(voxels) => {
            payload.push_u64(usize_to_u64(voxels.len()));
            for &voxel in voxels {
                push_point_payload(payload, voxel);
            }
        }
    }
}

fn push_recipient_payload(payload: &mut CanonicalPayloadBuilder, recipients: &CommandRecipients) {
    match recipients {
        CommandRecipients::All => {
            payload.push_u16(0);
        }
        CommandRecipients::Selected(ids) => {
            payload.push_u16(1).push_u64(usize_to_u64(ids.len()));
            for &id in ids {
                payload.push_u64(id);
            }
        }
        CommandRecipients::Group(id) => {
            payload.push_u16(2).push_u64(*id);
        }
    }
}

fn push_point_payload(payload: &mut CanonicalPayloadBuilder, point: IVec3) {
    payload
        .push_i32(point.x)
        .push_i32(point.y)
        .push_i32(point.z);
}

fn preview_issue_diagnostic(issue: &PreviewIssue) -> CreatorDiagnostic {
    let severity = match issue.severity() {
        IssueSeverity::Warning => DiagnosticSeverity::Warning,
        IssueSeverity::Error => DiagnosticSeverity::Error,
    };
    let (code, message) = match issue {
        PreviewIssue::EmptySelectedRecipients => (
            "recipients.empty",
            "selected bot recipient list is empty".to_owned(),
        ),
        PreviewIssue::DuplicateSelectedRecipient(id) => (
            "recipients.duplicate",
            format!("bot recipient {id} appears more than once"),
        ),
        PreviewIssue::InvalidArea { min, max } => (
            "target.invalid_area",
            format!("area bounds are inverted from {min:?} to {max:?}"),
        ),
        PreviewIssue::EmptyPath => ("target.empty_path", "command path is empty".to_owned()),
        PreviewIssue::SinglePointPath => (
            "target.single_point_path",
            "command path has only one point".to_owned(),
        ),
        PreviewIssue::EmptySelection => (
            "target.empty_selection",
            "voxel selection is empty".to_owned(),
        ),
        PreviewIssue::DuplicateSelectionVoxel(voxel) => (
            "target.duplicate_voxel",
            format!("voxel {voxel:?} appears more than once"),
        ),
        PreviewIssue::VoxelCostSaturated => (
            "cost.voxel_saturated",
            "voxel cost exceeded deterministic integer capacity".to_owned(),
        ),
        PreviewIssue::ChunkCostSaturated => (
            "cost.chunk_saturated",
            "chunk cost exceeded deterministic integer capacity".to_owned(),
        ),
        PreviewIssue::VoxelLimitExceeded { estimated, limit } => (
            "cost.voxel_limit",
            format!("estimated voxel cost {estimated} exceeds configured hard limit {limit}"),
        ),
        PreviewIssue::ChunkLimitExceeded { estimated, limit } => (
            "cost.chunk_limit",
            format!("estimated chunk cost {estimated} exceeds configured hard limit {limit}"),
        ),
    };
    CreatorDiagnostic::new(code, severity, message)
}

fn estimate_preview(
    operation: CommandOperation,
    target: &CommandTarget,
    recipients: &CommandRecipients,
    limits: PreviewLimits,
) -> CommandPreview {
    let mut issues = recipient_issues(recipients);
    let (target_voxels, target_chunks, mut target_issues) = target_cost(target, limits.chunk_edge);
    issues.append(&mut target_issues);

    let estimated_voxel_cost = match target_voxels.checked_mul(operation.voxel_cost_factor()) {
        Some(cost) => cost,
        None => {
            if !issues.contains(&PreviewIssue::VoxelCostSaturated) {
                issues.push(PreviewIssue::VoxelCostSaturated);
            }
            u64::MAX
        }
    };

    if estimated_voxel_cost > limits.max_voxel_cost {
        issues.push(PreviewIssue::VoxelLimitExceeded {
            estimated: estimated_voxel_cost,
            limit: limits.max_voxel_cost,
        });
    }
    if target_chunks > limits.max_chunk_cost {
        issues.push(PreviewIssue::ChunkLimitExceeded {
            estimated: target_chunks,
            limit: limits.max_chunk_cost,
        });
    }

    CommandPreview {
        estimated_voxel_cost,
        estimated_chunk_cost: target_chunks,
        issues,
    }
}

fn recipient_issues(recipients: &CommandRecipients) -> Vec<PreviewIssue> {
    let CommandRecipients::Selected(ids) = recipients else {
        return Vec::new();
    };

    if ids.is_empty() {
        return vec![PreviewIssue::EmptySelectedRecipients];
    }

    let mut seen = BTreeSet::new();
    let mut issues = Vec::new();
    for &id in ids {
        if !seen.insert(id) {
            issues.push(PreviewIssue::DuplicateSelectedRecipient(id));
        }
    }
    issues
}

fn target_cost(target: &CommandTarget, chunk_edge: u32) -> (u64, u64, Vec<PreviewIssue>) {
    match target {
        CommandTarget::Point(point) => (
            1,
            chunks_for_bounds(*point, *point, chunk_edge).0,
            Vec::new(),
        ),
        CommandTarget::Area { min, max } => {
            if min.x > max.x || min.y > max.y || min.z > max.z {
                return (
                    0,
                    0,
                    vec![PreviewIssue::InvalidArea {
                        min: *min,
                        max: *max,
                    }],
                );
            }

            let (voxels, voxel_overflow) = inclusive_volume(*min, *max);
            let (chunks, chunk_overflow) = chunks_for_bounds(*min, *max, chunk_edge);
            let mut issues = Vec::new();
            if voxel_overflow {
                issues.push(PreviewIssue::VoxelCostSaturated);
            }
            if chunk_overflow {
                issues.push(PreviewIssue::ChunkCostSaturated);
            }
            (voxels, chunks, issues)
        }
        CommandTarget::Path(points) => path_cost(points, chunk_edge),
        CommandTarget::Selection(voxels) => selection_cost(voxels, chunk_edge),
    }
}

fn path_cost(points: &[IVec3], chunk_edge: u32) -> (u64, u64, Vec<PreviewIssue>) {
    match points {
        [] => (0, 0, vec![PreviewIssue::EmptyPath]),
        [point] => (
            1,
            chunks_for_bounds(*point, *point, chunk_edge).0,
            vec![PreviewIssue::SinglePointPath],
        ),
        _ => {
            let mut voxel_cost = 1_u64;
            let mut saturated = false;
            for segment in points.windows(2) {
                let distance = manhattan_distance(segment[0], segment[1]);
                match voxel_cost.checked_add(distance) {
                    Some(cost) => voxel_cost = cost,
                    None => {
                        voxel_cost = u64::MAX;
                        saturated = true;
                        break;
                    }
                }
            }

            let (min, max) = bounds(points).expect("non-empty path has bounds");
            let (chunk_cost, chunk_overflow) = chunks_for_bounds(min, max, chunk_edge);
            let mut issues = Vec::new();
            if saturated {
                issues.push(PreviewIssue::VoxelCostSaturated);
            }
            if chunk_overflow {
                issues.push(PreviewIssue::ChunkCostSaturated);
            }
            (voxel_cost, chunk_cost, issues)
        }
    }
}

fn selection_cost(voxels: &[IVec3], chunk_edge: u32) -> (u64, u64, Vec<PreviewIssue>) {
    if voxels.is_empty() {
        return (0, 0, vec![PreviewIssue::EmptySelection]);
    }

    let edge = i64::from(chunk_edge);
    let mut unique_voxels = BTreeSet::new();
    let mut unique_chunks = BTreeSet::new();
    let mut issues = Vec::new();

    for &voxel in voxels {
        let key = point_key(voxel);
        if !unique_voxels.insert(key) {
            issues.push(PreviewIssue::DuplicateSelectionVoxel(voxel));
        }
        unique_chunks.insert((
            i64::from(voxel.x).div_euclid(edge),
            i64::from(voxel.y).div_euclid(edge),
            i64::from(voxel.z).div_euclid(edge),
        ));
    }

    (
        usize_to_u64(unique_voxels.len()),
        usize_to_u64(unique_chunks.len()),
        issues,
    )
}

fn inclusive_volume(min: IVec3, max: IVec3) -> (u64, bool) {
    let x = inclusive_extent(min.x, max.x);
    let y = inclusive_extent(min.y, max.y);
    let z = inclusive_extent(min.z, max.z);
    checked_product3(x, y, z)
}

fn chunks_for_bounds(min: IVec3, max: IVec3, chunk_edge: u32) -> (u64, bool) {
    let edge = i64::from(chunk_edge);
    let min_x = i64::from(min.x).div_euclid(edge);
    let min_y = i64::from(min.y).div_euclid(edge);
    let min_z = i64::from(min.z).div_euclid(edge);
    let max_x = i64::from(max.x).div_euclid(edge);
    let max_y = i64::from(max.y).div_euclid(edge);
    let max_z = i64::from(max.z).div_euclid(edge);
    checked_product3(
        (max_x - min_x + 1) as u64,
        (max_y - min_y + 1) as u64,
        (max_z - min_z + 1) as u64,
    )
}

fn checked_product3(a: u64, b: u64, c: u64) -> (u64, bool) {
    match a.checked_mul(b).and_then(|value| value.checked_mul(c)) {
        Some(value) => (value, false),
        None => (u64::MAX, true),
    }
}

fn inclusive_extent(min: i32, max: i32) -> u64 {
    (i64::from(max) - i64::from(min) + 1) as u64
}

fn manhattan_distance(a: IVec3, b: IVec3) -> u64 {
    i64::from(a.x).abs_diff(i64::from(b.x))
        + i64::from(a.y).abs_diff(i64::from(b.y))
        + i64::from(a.z).abs_diff(i64::from(b.z))
}

fn bounds(points: &[IVec3]) -> Option<(IVec3, IVec3)> {
    let (&first, rest) = points.split_first()?;
    let mut min = first;
    let mut max = first;
    for &point in rest {
        min = min.min(point);
        max = max.max(point);
    }
    Some((min, max))
}

const fn point_key(point: IVec3) -> (i32, i32, i32) {
    (point.x, point.y, point.z)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: i32, y: i32, z: i32) -> CommandTarget {
        CommandTarget::Point(IVec3::new(x, y, z))
    }

    fn draft(machine: &mut BotCommandStateMachine) -> CommandId {
        machine
            .create(
                CommandOperation::Pencil,
                point(1, 2, 3),
                CommandRecipients::All,
            )
            .unwrap()
    }

    fn approved(machine: &mut BotCommandStateMachine) -> CommandId {
        let id = draft(machine);
        machine.prepare_preview(id).unwrap();
        machine.approve(id).unwrap();
        id
    }

    fn assert_approved_tamper_rejected(
        mutate: impl FnOnce(&mut ApprovedCommand),
        expected: CommitAuthorizationError,
    ) {
        let mut machine = BotCommandStateMachine::default();
        let id = approved(&mut machine);
        mutate(
            machine
                .commands
                .get_mut(&id)
                .unwrap()
                .approved
                .as_mut()
                .unwrap(),
        );

        assert_eq!(
            machine.execute(id),
            Err(CommandError::AuthorizationFailed {
                id,
                reason: expected,
            })
        );
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Approved);
    }

    fn assert_resource<T: Resource>() {}

    #[test]
    fn state_machine_is_a_bevy_resource() {
        assert_resource::<BotCommandStateMachine>();
    }

    #[test]
    fn ids_are_monotonic_and_deterministic() {
        let mut first = BotCommandStateMachine::default();
        let mut second = BotCommandStateMachine::default();

        let first_ids = [draft(&mut first), draft(&mut first), draft(&mut first)];
        let second_ids = [draft(&mut second), draft(&mut second), draft(&mut second)];

        assert_eq!(first_ids, second_ids);
        assert_eq!(first_ids.map(CommandId::get), [1, 2, 3]);
        assert_eq!(
            first.commands().map(BotCommand::id).collect::<Vec<_>>(),
            first_ids
        );
    }

    #[test]
    fn identical_commands_issue_identical_preview_receipts() {
        let mut first = BotCommandStateMachine::default();
        let mut second = BotCommandStateMachine::default();
        let first_id = approved(&mut first);
        let second_id = approved(&mut second);

        let first_receipt = &first.command(first_id).unwrap().approved().unwrap().receipt;
        let second_receipt = &second
            .command(second_id)
            .unwrap()
            .approved()
            .unwrap()
            .receipt;

        assert_eq!(first_receipt, second_receipt);
        assert_eq!(first_receipt.short_code(), second_receipt.short_code());
    }

    #[test]
    fn invalid_chunk_edge_is_rejected() {
        assert_eq!(
            BotCommandStateMachine::new(PreviewLimits::new(0, 10, 10)).unwrap_err(),
            CommandError::InvalidPreviewConfiguration { chunk_edge: 0 }
        );
    }

    #[test]
    fn every_operation_has_a_stable_voxel_factor() {
        let cases = [
            (CommandOperation::Inspect, 0),
            (CommandOperation::Pencil, 1),
            (CommandOperation::Rectangle, 1),
            (CommandOperation::PushPull, 2),
            (CommandOperation::Room, 4),
            (CommandOperation::CutOpening, 1),
            (CommandOperation::Move, 2),
            (CommandOperation::Rotate, 2),
            (CommandOperation::Scale, 3),
            (CommandOperation::Paint, 1),
            (CommandOperation::Road, 3),
            (CommandOperation::ClearFlatten, 2),
            (CommandOperation::Repair, 2),
        ];

        for (operation, expected) in cases {
            assert_eq!(operation.voxel_cost_factor(), expected);
        }
    }

    #[test]
    fn point_preview_has_one_chunk_and_weighted_voxel_cost() {
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(
                CommandOperation::Room,
                point(-1, 32, 15),
                CommandRecipients::Group(9),
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(preview.estimated_voxel_cost, 4);
        assert_eq!(preview.estimated_chunk_cost, 1);
        assert!(preview.issues.is_empty());
    }

    #[test]
    fn inclusive_area_cost_crosses_negative_chunk_boundaries_correctly() {
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(
                CommandOperation::Rectangle,
                CommandTarget::Area {
                    min: IVec3::new(-16, 0, -1),
                    max: IVec3::new(16, 0, 16),
                },
                CommandRecipients::All,
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(preview.estimated_voxel_cost, 33 * 18);
        assert_eq!(preview.estimated_chunk_cost, 9);
    }

    #[test]
    fn path_cost_uses_manhattan_segments_and_chunk_bounds() {
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(
                CommandOperation::Road,
                CommandTarget::Path(vec![
                    IVec3::new(0, 0, 0),
                    IVec3::new(2, 0, 0),
                    IVec3::new(2, 3, 0),
                ]),
                CommandRecipients::All,
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(preview.estimated_voxel_cost, 6 * 3);
        assert_eq!(preview.estimated_chunk_cost, 1);
    }

    #[test]
    fn selection_deduplicates_cost_but_reports_duplicate_in_input_order() {
        let repeated = IVec3::new(1, 2, 3);
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(
                CommandOperation::Move,
                CommandTarget::Selection(vec![repeated, IVec3::new(16, 2, 3), repeated, repeated]),
                CommandRecipients::All,
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(preview.estimated_voxel_cost, 4);
        assert_eq!(preview.estimated_chunk_cost, 2);
        assert_eq!(
            preview.issues,
            vec![
                PreviewIssue::DuplicateSelectionVoxel(repeated),
                PreviewIssue::DuplicateSelectionVoxel(repeated),
            ]
        );
        assert!(!preview.has_errors());
    }

    #[test]
    fn preview_reports_recipient_and_target_issues_deterministically() {
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(
                CommandOperation::Inspect,
                CommandTarget::Path(Vec::new()),
                CommandRecipients::Selected(Vec::new()),
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(
            preview.issues,
            vec![
                PreviewIssue::EmptySelectedRecipients,
                PreviewIssue::EmptyPath,
            ]
        );
        assert!(preview.has_errors());
    }

    #[test]
    fn duplicate_recipients_block_approval() {
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(
                CommandOperation::Inspect,
                point(0, 0, 0),
                CommandRecipients::Selected(vec![7, 4, 7, 4]),
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(
            preview.issues,
            vec![
                PreviewIssue::DuplicateSelectedRecipient(7),
                PreviewIssue::DuplicateSelectedRecipient(4),
            ]
        );
        assert!(preview.has_errors());
        assert_eq!(machine.approve(id), Err(CommandError::PreviewHasErrors(id)));
    }

    #[test]
    fn empty_and_malformed_targets_block_approval() {
        let targets = [
            CommandTarget::Area {
                min: IVec3::new(2, 0, 0),
                max: IVec3::new(1, 0, 0),
            },
            CommandTarget::Path(Vec::new()),
            CommandTarget::Selection(Vec::new()),
        ];

        for target in targets {
            let mut machine = BotCommandStateMachine::default();
            let id = machine
                .create(CommandOperation::Repair, target, CommandRecipients::All)
                .unwrap();
            machine.prepare_preview(id).unwrap();
            assert_eq!(machine.approve(id), Err(CommandError::PreviewHasErrors(id)));
            assert_eq!(
                machine.command(id).unwrap().state(),
                CommandState::PreviewReady
            );
        }
    }

    #[test]
    fn configured_limits_are_hard_approval_boundaries() {
        let mut machine = BotCommandStateMachine::new(PreviewLimits::new(4, 10, 1)).unwrap();
        let id = machine
            .create(
                CommandOperation::Scale,
                CommandTarget::Area {
                    min: IVec3::ZERO,
                    max: IVec3::new(4, 1, 1),
                },
                CommandRecipients::All,
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(preview.estimated_voxel_cost, 60);
        assert_eq!(preview.estimated_chunk_cost, 2);
        assert_eq!(
            preview.issues,
            vec![
                PreviewIssue::VoxelLimitExceeded {
                    estimated: 60,
                    limit: 10,
                },
                PreviewIssue::ChunkLimitExceeded {
                    estimated: 2,
                    limit: 1,
                },
            ]
        );
        assert_eq!(machine.approve(id), Err(CommandError::PreviewHasErrors(id)));
        assert_eq!(
            machine.command(id).unwrap().state(),
            CommandState::PreviewReady
        );
    }

    #[test]
    fn changing_a_previewed_command_returns_it_to_draft() {
        let mut machine = BotCommandStateMachine::default();
        let id = draft(&mut machine);
        machine.prepare_preview(id).unwrap();
        let initial_revision = machine.command(id).unwrap().revision();

        machine.set_operation(id, CommandOperation::Paint).unwrap();
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Draft);
        assert_eq!(machine.command(id).unwrap().preview(), None);
        assert_eq!(
            machine.command(id).unwrap().revision().get(),
            initial_revision.get() + 1
        );

        machine
            .set_target(id, CommandTarget::Selection(vec![IVec3::ZERO]))
            .unwrap();
        machine
            .set_recipients(id, CommandRecipients::Group(3))
            .unwrap();
        assert_eq!(
            machine.command(id).unwrap().target(),
            &CommandTarget::Selection(vec![IVec3::ZERO])
        );
        assert_eq!(
            machine.command(id).unwrap().recipients(),
            &CommandRecipients::Group(3)
        );
        assert_eq!(
            machine.command(id).unwrap().revision().get(),
            initial_revision.get() + 3
        );
    }

    #[test]
    fn no_op_edits_preserve_preview_state_and_revision() {
        let mut machine = BotCommandStateMachine::default();
        let id = draft(&mut machine);
        let preview = machine.prepare_preview(id).unwrap().clone();
        let revision = machine.command(id).unwrap().revision();

        machine.set_operation(id, CommandOperation::Pencil).unwrap();
        machine.set_target(id, point(1, 2, 3)).unwrap();
        machine.set_recipients(id, CommandRecipients::All).unwrap();

        let command = machine.command(id).unwrap();
        assert_eq!(command.state(), CommandState::PreviewReady);
        assert_eq!(command.revision(), revision);
        assert_eq!(command.preview(), Some(&preview));
    }

    #[test]
    fn approval_freezes_exact_target_and_recipients() {
        let target = CommandTarget::Path(vec![IVec3::ZERO, IVec3::new(5, 1, -2)]);
        let recipients = CommandRecipients::Selected(vec![8, 2, 5]);
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(CommandOperation::Road, target.clone(), recipients.clone())
            .unwrap();
        machine.prepare_preview(id).unwrap();
        let snapshot = machine.approve(id).unwrap().clone();
        let approved_preview = machine.command(id).unwrap().preview().unwrap().clone();
        let receipt = snapshot.receipt.clone();

        assert_eq!(
            snapshot,
            ApprovedCommand {
                operation: CommandOperation::Road,
                target: target.clone(),
                recipients: recipients.clone(),
                preview: approved_preview,
                limits: machine.limits(),
                receipt,
            }
        );
        assert_eq!(
            machine.set_target(id, point(99, 99, 99)),
            Err(CommandError::CommandFrozen(id))
        );
        assert_eq!(
            machine.set_recipients(id, CommandRecipients::All),
            Err(CommandError::CommandFrozen(id))
        );
        assert_eq!(
            machine.set_operation(id, CommandOperation::Inspect),
            Err(CommandError::CommandFrozen(id))
        );
        assert_eq!(machine.command(id).unwrap().target(), &target);
        assert_eq!(machine.command(id).unwrap().recipients(), &recipients);
        assert_eq!(machine.command(id).unwrap().approved(), Some(&snapshot));
    }

    #[test]
    fn execution_rejects_any_approved_content_tampering() {
        assert_approved_tamper_rejected(
            |approved| approved.operation = CommandOperation::Paint,
            CommitAuthorizationError::ContentMismatch,
        );
        assert_approved_tamper_rejected(
            |approved| approved.target = point(99, 1, -4),
            CommitAuthorizationError::ContentMismatch,
        );
        assert_approved_tamper_rejected(
            |approved| approved.recipients = CommandRecipients::Group(5),
            CommitAuthorizationError::ContentMismatch,
        );
        assert_approved_tamper_rejected(
            |approved| approved.limits.max_voxel_cost += 1,
            CommitAuthorizationError::ContentMismatch,
        );
    }

    #[test]
    fn execution_rejects_approved_cost_or_diagnostic_tampering() {
        assert_approved_tamper_rejected(
            |approved| approved.preview.estimated_voxel_cost += 1,
            CommitAuthorizationError::CostFingerprintMismatch,
        );
        assert_approved_tamper_rejected(
            |approved| {
                approved.preview.issues.push(PreviewIssue::SinglePointPath);
            },
            CommitAuthorizationError::DiagnosticsMismatch,
        );
    }

    #[test]
    fn execution_revalidates_the_live_command_against_approval() {
        let cases = [
            CommitAuthorizationError::RevisionMismatch,
            CommitAuthorizationError::ContentMismatch,
            CommitAuthorizationError::DiagnosticsMismatch,
            CommitAuthorizationError::CostFingerprintMismatch,
        ];

        for expected in cases {
            let mut machine = BotCommandStateMachine::default();
            let id = approved(&mut machine);
            let command = machine.commands.get_mut(&id).unwrap();
            match expected {
                CommitAuthorizationError::RevisionMismatch => command.revision.next(),
                CommitAuthorizationError::ContentMismatch => {
                    command.operation = CommandOperation::Paint;
                }
                CommitAuthorizationError::DiagnosticsMismatch => {
                    command
                        .preview
                        .as_mut()
                        .unwrap()
                        .issues
                        .push(PreviewIssue::SinglePointPath);
                }
                CommitAuthorizationError::CostFingerprintMismatch => {
                    command.preview.as_mut().unwrap().estimated_chunk_cost += 1;
                }
                _ => unreachable!("test only covers mutable live command bindings"),
            }

            assert_eq!(
                machine.execute(id),
                Err(CommandError::AuthorizationFailed {
                    id,
                    reason: expected,
                })
            );
            assert_eq!(machine.command(id).unwrap().state(), CommandState::Approved);
        }
    }

    #[test]
    fn execution_is_rejected_before_approval_from_draft_and_preview() {
        let mut machine = BotCommandStateMachine::default();
        let id = draft(&mut machine);

        assert_eq!(
            machine.execute(id),
            Err(CommandError::ExecuteBeforeApproval(id))
        );
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Draft);

        machine.prepare_preview(id).unwrap();
        assert_eq!(
            machine.execute(id),
            Err(CommandError::ExecuteBeforeApproval(id))
        );
        assert_eq!(
            machine.command(id).unwrap().state(),
            CommandState::PreviewReady
        );
    }

    #[test]
    fn approved_command_runs_pauses_resumes_and_completes() {
        let mut machine = BotCommandStateMachine::default();
        let id = approved(&mut machine);

        machine.execute(id).unwrap();
        let permit = machine
            .claim_dispatch(id)
            .unwrap()
            .expect("first claim issues permit");
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Running);
        machine.pause(id).unwrap();
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Paused);
        assert!(machine.claim_dispatch(id).unwrap().is_none());
        machine.resume(id).unwrap();
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Running);
        assert!(machine.claim_dispatch(id).unwrap().is_none());
        let summary = CompletionSummary {
            applied_voxel_edits: 1,
            touched_chunks: 1,
            spawned_projects: 1,
        };
        machine.complete_dispatch(&permit, summary).unwrap();
        assert_eq!(
            machine.command(id).unwrap().state(),
            CommandState::Completed
        );
        assert_eq!(machine.command(id).unwrap().completion(), Some(summary));
        assert!(machine.command(id).unwrap().state().is_terminal());
    }

    #[test]
    fn dispatch_claim_is_issued_exactly_once() {
        let mut machine = BotCommandStateMachine::default();
        let id = approved(&mut machine);
        machine.request_execution(id).unwrap();

        let permit = machine.claim_dispatch(id).unwrap().expect("first claim");
        assert_eq!(permit.key().command_id, id);
        assert_eq!(
            permit.key().approval_fingerprint,
            permit.approved().receipt.binding_fingerprint()
        );
        assert_eq!(
            machine.command(id).unwrap().dispatch_key(),
            Some(permit.key())
        );
        assert!(machine.claim_dispatch(id).unwrap().is_none());
    }

    #[test]
    fn cancelled_command_never_dispatches_or_redispatches() {
        let mut machine = BotCommandStateMachine::default();
        let before_claim = approved(&mut machine);
        machine.cancel(before_claim).unwrap();
        assert!(machine.claim_dispatch(before_claim).unwrap().is_none());

        let after_claim = approved(&mut machine);
        machine.request_execution(after_claim).unwrap();
        let permit = machine
            .claim_dispatch(after_claim)
            .unwrap()
            .expect("permit");
        machine.cancel(after_claim).unwrap();
        assert!(machine.claim_dispatch(after_claim).unwrap().is_none());
        assert!(matches!(
            machine.complete_dispatch(&permit, CompletionSummary::default()),
            Err(CommandError::InvalidTransition {
                from: CommandState::Cancelled,
                to: CommandState::Completed,
                ..
            })
        ));
    }

    #[test]
    fn completion_cannot_exceed_the_approved_cost() {
        let mut machine = BotCommandStateMachine::default();
        let id = approved(&mut machine);
        machine.request_execution(id).unwrap();
        let permit = machine
            .claim_dispatch(id)
            .unwrap()
            .expect("dispatch permit");
        let approved_voxel_cost = permit.approved().preview.estimated_voxel_cost;
        let approved_chunk_cost = permit.approved().preview.estimated_chunk_cost;

        assert_eq!(
            machine.complete_dispatch(
                &permit,
                CompletionSummary {
                    applied_voxel_edits: approved_voxel_cost.saturating_add(1),
                    touched_chunks: approved_chunk_cost,
                    spawned_projects: 1,
                },
            ),
            Err(CommandError::CompletionCostExceeded {
                id,
                applied_voxel_edits: approved_voxel_cost.saturating_add(1),
                approved_voxel_cost,
                touched_chunks: approved_chunk_cost,
                approved_chunk_cost,
            })
        );
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Running);
        assert_eq!(machine.command(id).unwrap().completion(), None);
    }

    #[test]
    fn completion_waits_until_a_paused_job_resumes() {
        let mut machine = BotCommandStateMachine::default();
        let id = approved(&mut machine);
        machine.request_execution(id).unwrap();
        let permit = machine
            .claim_dispatch(id)
            .unwrap()
            .expect("dispatch permit");
        machine.pause(id).unwrap();

        assert!(matches!(
            machine.complete_dispatch(&permit, CompletionSummary::default()),
            Err(CommandError::InvalidTransition {
                from: CommandState::Paused,
                to: CommandState::Completed,
                ..
            })
        ));
        machine.resume(id).unwrap();
        machine
            .complete_dispatch(&permit, CompletionSummary::default())
            .unwrap();
        assert_eq!(
            machine.command(id).unwrap().state(),
            CommandState::Completed
        );
    }

    #[test]
    fn invalid_lifecycle_transitions_are_rejected_without_mutation() {
        let mut machine = BotCommandStateMachine::default();
        let id = approved(&mut machine);

        assert_eq!(
            machine.pause(id),
            Err(CommandError::InvalidTransition {
                id,
                from: CommandState::Approved,
                to: CommandState::Paused,
            })
        );
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Approved);

        machine.execute(id).unwrap();
        assert_eq!(
            machine.approve(id),
            Err(CommandError::InvalidTransition {
                id,
                from: CommandState::Running,
                to: CommandState::Approved,
            })
        );
        assert_eq!(
            machine.resume(id),
            Err(CommandError::InvalidTransition {
                id,
                from: CommandState::Running,
                to: CommandState::Running,
            })
        );
        assert_eq!(machine.command(id).unwrap().state(), CommandState::Running);
    }

    #[test]
    fn terminal_states_reject_further_transitions() {
        let mut completed_machine = BotCommandStateMachine::default();
        let completed = approved(&mut completed_machine);
        completed_machine.execute(completed).unwrap();
        let permit = completed_machine
            .claim_dispatch(completed)
            .unwrap()
            .expect("dispatch permit");
        completed_machine
            .complete_dispatch(&permit, CompletionSummary::default())
            .unwrap();

        assert!(matches!(
            completed_machine.cancel(completed),
            Err(CommandError::InvalidTransition {
                from: CommandState::Completed,
                ..
            })
        ));
        assert!(matches!(
            completed_machine.execute(completed),
            Err(CommandError::InvalidTransition {
                from: CommandState::Completed,
                ..
            })
        ));

        let mut cancelled_machine = BotCommandStateMachine::default();
        let cancelled = draft(&mut cancelled_machine);
        cancelled_machine.cancel(cancelled).unwrap();
        assert!(matches!(
            cancelled_machine.prepare_preview(cancelled),
            Err(CommandError::InvalidTransition {
                from: CommandState::Cancelled,
                ..
            })
        ));
    }

    #[test]
    fn cancellation_is_valid_before_and_after_approval() {
        let mut draft_machine = BotCommandStateMachine::default();
        let draft_id = draft(&mut draft_machine);
        draft_machine.cancel(draft_id).unwrap();
        assert_eq!(
            draft_machine.command(draft_id).unwrap().state(),
            CommandState::Cancelled
        );

        let mut paused_machine = BotCommandStateMachine::default();
        let paused_id = approved(&mut paused_machine);
        paused_machine.execute(paused_id).unwrap();
        paused_machine.pause(paused_id).unwrap();
        paused_machine.cancel(paused_id).unwrap();
        assert_eq!(
            paused_machine.command(paused_id).unwrap().state(),
            CommandState::Cancelled
        );
    }

    #[test]
    fn blocking_requires_a_reason_and_restores_interrupted_state() {
        for interrupted in [
            CommandState::Approved,
            CommandState::Running,
            CommandState::Paused,
        ] {
            let mut machine = BotCommandStateMachine::default();
            let id = approved(&mut machine);
            if matches!(interrupted, CommandState::Running | CommandState::Paused) {
                machine.execute(id).unwrap();
            }
            if interrupted == CommandState::Paused {
                machine.pause(id).unwrap();
            }

            assert_eq!(
                machine.block(id, "   "),
                Err(CommandError::MissingBlockReason(id))
            );
            assert_eq!(machine.command(id).unwrap().state(), interrupted);

            machine.block(id, "waiting for a clear work zone").unwrap();
            let command = machine.command(id).unwrap();
            assert_eq!(command.state(), CommandState::Blocked);
            assert_eq!(
                command.block_reason(),
                Some("waiting for a clear work zone")
            );
            assert!(command.approved().is_some());

            machine.unblock(id).unwrap();
            let command = machine.command(id).unwrap();
            assert_eq!(command.state(), interrupted);
            assert_eq!(command.block_reason(), None);
        }
    }

    #[test]
    fn draft_cannot_be_blocked_or_unblocked() {
        let mut machine = BotCommandStateMachine::default();
        let id = draft(&mut machine);

        assert!(matches!(
            machine.block(id, "not approved"),
            Err(CommandError::InvalidTransition {
                from: CommandState::Draft,
                to: CommandState::Blocked,
                ..
            })
        ));
        assert!(matches!(
            machine.unblock(id),
            Err(CommandError::MissingBlockedState(command_id)) if command_id == id
        ));
    }

    #[test]
    fn blocked_command_remains_frozen_and_can_be_cancelled() {
        let mut machine = BotCommandStateMachine::default();
        let id = approved(&mut machine);
        let frozen = machine.command(id).unwrap().approved().unwrap().clone();

        machine.block(id, "worker unavailable").unwrap();
        assert_eq!(
            machine.set_target(id, point(10, 10, 10)),
            Err(CommandError::CommandFrozen(id))
        );
        assert_eq!(machine.command(id).unwrap().approved(), Some(&frozen));

        machine.cancel(id).unwrap();
        assert_eq!(
            machine.command(id).unwrap().state(),
            CommandState::Cancelled
        );
    }

    #[test]
    fn unknown_ids_are_rejected_consistently() {
        let mut machine = BotCommandStateMachine::default();
        let unknown = CommandId(55);
        assert_eq!(
            machine.command(unknown),
            Err(CommandError::UnknownCommand(unknown))
        );
        assert_eq!(
            machine.execute(unknown),
            Err(CommandError::UnknownCommand(unknown))
        );
    }

    #[test]
    fn cost_is_repeatable_across_repreview_and_independent_machines() {
        let target = CommandTarget::Area {
            min: IVec3::new(-20, -2, 7),
            max: IVec3::new(40, 5, 33),
        };
        let limits = PreviewLimits::new(16, u64::MAX, u64::MAX);
        let mut first = BotCommandStateMachine::new(limits).unwrap();
        let mut second = BotCommandStateMachine::new(limits).unwrap();
        let first_id = first
            .create(
                CommandOperation::ClearFlatten,
                target.clone(),
                CommandRecipients::Group(42),
            )
            .unwrap();
        let second_id = second
            .create(
                CommandOperation::ClearFlatten,
                target,
                CommandRecipients::Group(42),
            )
            .unwrap();

        let first_preview = first.prepare_preview(first_id).unwrap().clone();
        let repeated_preview = first.prepare_preview(first_id).unwrap().clone();
        let second_preview = second.prepare_preview(second_id).unwrap().clone();
        assert_eq!(first_preview, repeated_preview);
        assert_eq!(first_preview, second_preview);
    }

    #[test]
    fn extreme_area_saturates_cost_and_blocks_approval() {
        let mut machine =
            BotCommandStateMachine::new(PreviewLimits::new(1, u64::MAX, u64::MAX)).unwrap();
        let id = machine
            .create(
                CommandOperation::Room,
                CommandTarget::Area {
                    min: IVec3::splat(i32::MIN),
                    max: IVec3::splat(i32::MAX),
                },
                CommandRecipients::All,
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(preview.estimated_voxel_cost, u64::MAX);
        assert_eq!(preview.estimated_chunk_cost, u64::MAX);
        assert!(preview.issues.contains(&PreviewIssue::VoxelCostSaturated));
        assert!(preview.issues.contains(&PreviewIssue::ChunkCostSaturated));
        assert_eq!(machine.approve(id), Err(CommandError::PreviewHasErrors(id)));
    }

    #[test]
    fn inspect_has_zero_voxel_mutation_cost_but_tracks_chunks() {
        let mut machine = BotCommandStateMachine::default();
        let id = machine
            .create(
                CommandOperation::Inspect,
                CommandTarget::Selection(vec![
                    IVec3::new(-1, 0, 0),
                    IVec3::new(0, 0, 0),
                    IVec3::new(16, 0, 0),
                ]),
                CommandRecipients::All,
            )
            .unwrap();

        let preview = machine.prepare_preview(id).unwrap();
        assert_eq!(preview.estimated_voxel_cost, 0);
        assert_eq!(preview.estimated_chunk_cost, 3);
    }
}
