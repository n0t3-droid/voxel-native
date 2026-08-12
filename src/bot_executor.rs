//! Runtime bridge from approved bot commands to real, sliced world projects.
//!
//! The command state machine owns authorization. This resource owns the single
//! non-cloneable dispatch permit and correlates it with one exact bot project.
//! The guarantee is process-local: command/job persistence across a crash is a
//! separate milestone.

use std::collections::{BTreeMap, HashSet};

use bevy::prelude::*;

use crate::bot_command::{
    ApprovedCommand, BotCommandStateMachine, CommandId, CommandOperation, CommandRecipients,
    CommandState, CommandTarget, CompletionSummary, DispatchPermit,
};
use crate::bots::{
    add_project_for_exact_bots, retire_exact_bot_project, validate_project_for_exact_bots,
    BotProjectStatus, BotTaskKind, BotTheme, FriendlyWorldBrain,
};
use crate::chunk::{ChunkPos, CHUNK_SIZE};
use crate::player::Player;
use crate::ships::ShipInstance;
use crate::world::VoxelWorld;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactBotCommandPlan {
    ClearFlatten,
}

#[derive(Resource, Debug, Default)]
pub(crate) struct BotCommandExecutor {
    jobs: BTreeMap<CommandId, BotCommandJob>,
    project_to_command: BTreeMap<u64, CommandId>,
}

#[derive(Debug)]
struct BotCommandJob {
    permit: DispatchPermit,
    plan: ExactBotCommandPlan,
    project_id: Option<u64>,
    resume_status: Option<BotProjectStatus>,
    applied_voxel_edits: u64,
    touched_chunks: HashSet<ChunkPos>,
}

#[derive(Debug, Clone)]
struct ExactProjectSpec {
    plan: ExactBotCommandPlan,
    origin: [i32; 3],
    size: [i32; 3],
    bot_ids: Vec<u64>,
}

impl BotCommandExecutor {
    pub(crate) fn plan_for_project(&self, project_id: u64) -> Option<ExactBotCommandPlan> {
        let command_id = self.project_to_command.get(&project_id)?;
        self.jobs.get(command_id).map(|job| job.plan)
    }

    pub(crate) fn record_project_progress(
        &mut self,
        project_id: u64,
        changed: usize,
        touched_chunks: impl IntoIterator<Item = ChunkPos>,
    ) {
        let Some(command_id) = self.project_to_command.get(&project_id).copied() else {
            return;
        };
        let Some(job) = self.jobs.get_mut(&command_id) else {
            return;
        };
        job.applied_voxel_edits = job
            .applied_voxel_edits
            .saturating_add(u64::try_from(changed).unwrap_or(u64::MAX));
        job.touched_chunks.extend(touched_chunks);
    }

    fn insert_claim(&mut self, permit: DispatchPermit, plan: ExactBotCommandPlan) {
        let command_id = permit.key().command_id;
        let previous = self.jobs.insert(
            command_id,
            BotCommandJob {
                permit,
                plan,
                project_id: None,
                resume_status: None,
                applied_voxel_edits: 0,
                touched_chunks: HashSet::new(),
            },
        );
        debug_assert!(
            previous.is_none(),
            "a command may own only one executor job"
        );
    }

    fn attach_project(&mut self, command_id: CommandId, project_id: u64) {
        let job = self
            .jobs
            .get_mut(&command_id)
            .expect("claimed command job must exist before project attachment");
        debug_assert!(job.project_id.is_none());
        job.project_id = Some(project_id);
        self.project_to_command.insert(project_id, command_id);
    }

    fn remove_job(&mut self, command_id: CommandId) -> Option<BotCommandJob> {
        let job = self.jobs.remove(&command_id)?;
        if let Some(project_id) = job.project_id {
            self.project_to_command.remove(&project_id);
        }
        Some(job)
    }
}

pub(crate) fn dispatch_authorized_bot_commands(
    mut commands: ResMut<BotCommandStateMachine>,
    mut executor: ResMut<BotCommandExecutor>,
    mut brain: ResMut<FriendlyWorldBrain>,
    world: Res<VoxelWorld>,
    player_q: Query<&Transform, With<Player>>,
    ship_q: Query<&Transform, With<ShipInstance>>,
) {
    let player_pos = player_q
        .get_single()
        .ok()
        .map(|transform| transform.translation);
    let ship_positions = ship_q
        .iter()
        .map(|transform| transform.translation)
        .collect::<Vec<_>>();

    reconcile_existing_jobs(&mut commands, &mut executor, &mut brain);

    let candidates = commands
        .commands()
        .filter(|command| {
            command.state() == CommandState::Running
                && command.dispatch_key().is_none()
                && !executor.jobs.contains_key(&command.id())
        })
        .map(|command| command.id())
        .collect::<Vec<_>>();

    for command_id in candidates {
        let spec = match commands
            .command(command_id)
            .ok()
            .and_then(|command| command.approved())
            .map(exact_project_spec)
        {
            Some(Ok(spec)) => spec,
            Some(Err(reason)) => {
                block_running_command(&mut commands, command_id, &reason);
                brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
                continue;
            }
            None => {
                block_running_command(
                    &mut commands,
                    command_id,
                    "approved command snapshot is missing",
                );
                continue;
            }
        };

        if !brain.save.autonomy.bots_active {
            let reason = "bot workers are OFF; enable them before exact execution";
            block_running_command(&mut commands, command_id, reason);
            brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
            continue;
        }
        if let Err(reason) = validate_project_for_exact_bots(
            &brain.save,
            &world,
            BotTaskKind::ClearFlatten,
            spec.origin,
            spec.size,
            &spec.bot_ids,
            player_pos,
            &ship_positions,
        ) {
            block_running_command(&mut commands, command_id, &reason);
            brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
            continue;
        }

        match commands.claim_dispatch(command_id) {
            Ok(Some(permit)) => executor.insert_claim(permit, spec.plan),
            Ok(None) => continue,
            Err(error) => {
                let reason = format!("dispatch authorization failed: {error}");
                block_running_command(&mut commands, command_id, &reason);
                brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
                continue;
            }
        }

        attempt_project_dispatch(
            command_id,
            &mut commands,
            &mut executor,
            &mut brain,
            &world,
            player_pos,
            &ship_positions,
        );
    }

    let pending = executor
        .jobs
        .iter()
        .filter(|(command_id, job)| {
            job.project_id.is_none()
                && commands
                    .command(**command_id)
                    .is_ok_and(|command| command.state() == CommandState::Running)
        })
        .map(|(command_id, _)| *command_id)
        .collect::<Vec<_>>();
    for command_id in pending {
        attempt_project_dispatch(
            command_id,
            &mut commands,
            &mut executor,
            &mut brain,
            &world,
            player_pos,
            &ship_positions,
        );
    }
}

fn reconcile_existing_jobs(
    commands: &mut BotCommandStateMachine,
    executor: &mut BotCommandExecutor,
    brain: &mut FriendlyWorldBrain,
) {
    let states = executor
        .jobs
        .keys()
        .copied()
        .filter_map(|command_id| {
            commands
                .command(command_id)
                .ok()
                .map(|command| (command_id, command.state()))
        })
        .collect::<Vec<_>>();

    for (command_id, state) in states {
        match state {
            CommandState::Running => resume_project(command_id, executor, brain),
            CommandState::Paused => {
                hold_project(command_id, executor, brain, "command paused by controller")
            }
            CommandState::Blocked => {
                let reason = commands
                    .command(command_id)
                    .ok()
                    .and_then(|command| command.block_reason())
                    .unwrap_or("command blocked by controller");
                hold_project(command_id, executor, brain, reason);
            }
            CommandState::Cancelled => {
                retire_job(
                    command_id,
                    executor,
                    brain,
                    "command cancelled by controller",
                );
            }
            CommandState::Completed => {
                executor.remove_job(command_id);
            }
            CommandState::Draft | CommandState::PreviewReady | CommandState::Approved => {
                retire_job(
                    command_id,
                    executor,
                    brain,
                    "command authorization was withdrawn",
                );
            }
        }
    }
}

fn hold_project(
    command_id: CommandId,
    executor: &mut BotCommandExecutor,
    brain: &mut FriendlyWorldBrain,
    reason: &str,
) {
    let Some(job) = executor.jobs.get_mut(&command_id) else {
        return;
    };
    let Some(project_id) = job.project_id else {
        return;
    };
    let Some(project) = brain
        .save
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
    else {
        return;
    };
    if project.status != BotProjectStatus::CommandHeld {
        job.resume_status = Some(match project.status {
            BotProjectStatus::Blocked | BotProjectStatus::Complete => BotProjectStatus::Queued,
            status => status,
        });
    }
    project.status = BotProjectStatus::CommandHeld;
    project.blocked_reason = reason.to_owned();
}

fn resume_project(
    command_id: CommandId,
    executor: &mut BotCommandExecutor,
    brain: &mut FriendlyWorldBrain,
) {
    let Some(job) = executor.jobs.get_mut(&command_id) else {
        return;
    };
    let Some(project_id) = job.project_id else {
        return;
    };
    let Some(project) = brain
        .save
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
    else {
        return;
    };
    if project.status == BotProjectStatus::CommandHeld {
        project.status = job.resume_status.take().unwrap_or(BotProjectStatus::Queued);
        project.blocked_reason.clear();
    }
}

fn retire_job(
    command_id: CommandId,
    executor: &mut BotCommandExecutor,
    brain: &mut FriendlyWorldBrain,
    reason: &str,
) {
    let Some(job) = executor.remove_job(command_id) else {
        return;
    };
    if let Some(project_id) = job.project_id {
        retire_exact_bot_project(&mut brain.save, project_id, reason);
        brain.mark_dirty();
        brain.hud_message = format!("Agent command #{command_id} stopped: {reason}");
    }
}

#[allow(clippy::too_many_arguments)]
fn attempt_project_dispatch(
    command_id: CommandId,
    commands: &mut BotCommandStateMachine,
    executor: &mut BotCommandExecutor,
    brain: &mut FriendlyWorldBrain,
    world: &VoxelWorld,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
) {
    let Some(job) = executor.jobs.get(&command_id) else {
        return;
    };
    if job.project_id.is_some() {
        return;
    }
    let spec = match exact_project_spec(job.permit.approved()) {
        Ok(spec) => spec,
        Err(reason) => {
            block_running_command(commands, command_id, &reason);
            return;
        }
    };
    if !brain.save.autonomy.bots_active {
        let reason = "bot workers are OFF; enable them before exact execution";
        block_running_command(commands, command_id, reason);
        brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
        return;
    }

    match add_project_for_exact_bots(
        &mut brain.save,
        world,
        command_id.get(),
        BotTaskKind::ClearFlatten,
        spec.origin,
        spec.size,
        BotTheme::CyanAlloy,
        &spec.bot_ids,
        10,
        player_pos,
        ship_positions,
    ) {
        Ok(project_id) => {
            executor.attach_project(command_id, project_id);
            brain.mark_dirty();
            brain.hud_message =
                format!("Agent command #{command_id} dispatched as bot project #{project_id}");
        }
        Err(reason) => {
            block_running_command(commands, command_id, &reason);
            brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
        }
    }
}

fn block_running_command(
    commands: &mut BotCommandStateMachine,
    command_id: CommandId,
    reason: &str,
) {
    if commands
        .command(command_id)
        .is_ok_and(|command| command.state() == CommandState::Running)
    {
        let _ = commands.block(command_id, reason);
    }
}

pub(crate) fn cancel_bot_command_jobs_on_world_unload(
    mut commands: ResMut<BotCommandStateMachine>,
    mut executor: ResMut<BotCommandExecutor>,
    mut brain: ResMut<FriendlyWorldBrain>,
) {
    retire_all_bot_command_jobs(
        &mut commands,
        &mut executor,
        &mut brain,
        "world session ended before exact execution completed",
    );
}

pub(crate) fn retire_all_bot_command_jobs(
    commands: &mut BotCommandStateMachine,
    executor: &mut BotCommandExecutor,
    brain: &mut FriendlyWorldBrain,
    reason: &str,
) {
    let command_ids = executor.jobs.keys().copied().collect::<Vec<_>>();
    for command_id in command_ids {
        if commands.command(command_id).is_ok_and(|command| {
            matches!(
                command.state(),
                CommandState::Running | CommandState::Paused | CommandState::Blocked
            )
        }) {
            let _ = commands.cancel(command_id);
        }
        retire_job(command_id, executor, brain, reason);
    }
}

pub(crate) fn finalize_authorized_bot_commands(
    mut commands: ResMut<BotCommandStateMachine>,
    mut executor: ResMut<BotCommandExecutor>,
    mut brain: ResMut<FriendlyWorldBrain>,
) {
    enum FinalizeAction {
        Complete(CommandId),
        Block(CommandId, String),
        Missing(CommandId),
    }

    let actions = executor
        .jobs
        .iter()
        .filter_map(|(command_id, job)| {
            let project_id = job.project_id?;
            match brain
                .save
                .projects
                .iter()
                .find(|project| project.id == project_id)
            {
                Some(project) if project.status == BotProjectStatus::Complete => {
                    Some(FinalizeAction::Complete(*command_id))
                }
                Some(project) if project.status == BotProjectStatus::Blocked => Some(
                    FinalizeAction::Block(*command_id, project.blocked_reason.clone()),
                ),
                Some(_) => None,
                None => Some(FinalizeAction::Missing(*command_id)),
            }
        })
        .collect::<Vec<_>>();

    for action in actions {
        match action {
            FinalizeAction::Complete(command_id) => {
                let result = {
                    let job = executor
                        .jobs
                        .get(&command_id)
                        .expect("completion action came from an existing executor job");
                    commands.complete_dispatch(
                        &job.permit,
                        CompletionSummary {
                            applied_voxel_edits: job.applied_voxel_edits,
                            touched_chunks: job.touched_chunks.len() as u64,
                            spawned_projects: 1,
                        },
                    )
                };
                match result {
                    Ok(()) => {
                        executor.remove_job(command_id);
                        brain.hud_message =
                            format!("Agent command #{command_id} completed in the voxel world");
                    }
                    Err(error) => {
                        let reason = format!("executor completion rejected: {error}");
                        block_running_command(&mut commands, command_id, &reason);
                        brain.hud_message =
                            format!("Agent command #{command_id} blocked: {reason}");
                    }
                }
            }
            FinalizeAction::Block(command_id, reason) => {
                hold_project(command_id, &mut executor, &mut brain, &reason);
                block_running_command(&mut commands, command_id, &reason);
                brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
            }
            FinalizeAction::Missing(command_id) => {
                let reason =
                    "correlated bot project disappeared; redispatch is disabled to avoid duplicates";
                block_running_command(&mut commands, command_id, reason);
                brain.hud_message = format!("Agent command #{command_id} blocked: {reason}");
            }
        }
    }
}

fn exact_project_spec(approved: &ApprovedCommand) -> Result<ExactProjectSpec, String> {
    if approved.limits.chunk_edge as usize != CHUNK_SIZE {
        return Err(format!(
            "approved chunk edge {} does not match the world chunk edge {CHUNK_SIZE}",
            approved.limits.chunk_edge
        ));
    }
    if approved.operation != CommandOperation::ClearFlatten {
        return Err(format!(
            "operation {:?} has no exact world executor yet; ClearFlatten is supported",
            approved.operation
        ));
    }
    let (min, max) = match &approved.target {
        CommandTarget::Area { min, max } => (*min, *max),
        other => {
            return Err(format!(
                "target {:?} is not executable yet; an inclusive Area is required",
                other.kind()
            ))
        }
    };
    let bot_ids = match &approved.recipients {
        CommandRecipients::Selected(bot_ids) => bot_ids.clone(),
        CommandRecipients::All => {
            return Err("exact execution requires Selected bot recipients, not All".into())
        }
        CommandRecipients::Group(group_id) => {
            return Err(format!(
                "exact execution cannot expand group {group_id}; use Selected bot IDs"
            ))
        }
    };
    let size = inclusive_size(min, max)?;
    Ok(ExactProjectSpec {
        plan: ExactBotCommandPlan::ClearFlatten,
        origin: [min.x, min.y, min.z],
        size,
        bot_ids,
    })
}

fn inclusive_size(min: IVec3, max: IVec3) -> Result<[i32; 3], String> {
    if min.x > max.x || min.y > max.y || min.z > max.z {
        return Err("approved area bounds are inverted".into());
    }
    let extent = |low: i32, high: i32| {
        i64::from(high)
            .checked_sub(i64::from(low))
            .and_then(|value| value.checked_add(1))
            .and_then(|value| i32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| "approved area extent does not fit a bot project".to_owned())
    };
    Ok([
        extent(min.x, max.x)?,
        extent(min.y, max.y)?,
        extent(min.z, max.z)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_command::{CommandRecipients, PreviewLimits};
    use crate::menu::GameState;

    fn approved_clear_flatten(
        min: IVec3,
        max: IVec3,
        recipients: CommandRecipients,
    ) -> (BotCommandStateMachine, CommandId) {
        let mut commands =
            BotCommandStateMachine::new(PreviewLimits::new(16, 1_000_000, 4_096)).unwrap();
        let id = commands
            .create(
                CommandOperation::ClearFlatten,
                CommandTarget::Area { min, max },
                recipients,
            )
            .unwrap();
        commands.prepare_preview(id).unwrap();
        commands.approve(id).unwrap();
        commands.request_execution(id).unwrap();
        (commands, id)
    }

    #[test]
    fn exact_spec_preserves_inclusive_area_and_recipient_order() {
        let (commands, id) = approved_clear_flatten(
            IVec3::new(-3, 20, 7),
            IVec3::new(2, 22, 10),
            CommandRecipients::Selected(vec![9, 4, 7]),
        );

        let spec = exact_project_spec(commands.command(id).unwrap().approved().unwrap()).unwrap();

        assert_eq!(spec.origin, [-3, 20, 7]);
        assert_eq!(spec.size, [6, 3, 4]);
        assert_eq!(spec.bot_ids, vec![9, 4, 7]);
        assert_eq!(spec.plan, ExactBotCommandPlan::ClearFlatten);
    }

    #[test]
    fn unsupported_operation_is_blocked_before_claim() {
        let mut commands = BotCommandStateMachine::default();
        let id = commands
            .create(
                CommandOperation::Road,
                CommandTarget::Area {
                    min: IVec3::ZERO,
                    max: IVec3::new(2, 0, 2),
                },
                CommandRecipients::Selected(vec![1]),
            )
            .unwrap();
        commands.prepare_preview(id).unwrap();
        commands.approve(id).unwrap();
        commands.request_execution(id).unwrap();

        let reason =
            exact_project_spec(commands.command(id).unwrap().approved().unwrap()).unwrap_err();
        block_running_command(&mut commands, id, &reason);

        let command = commands.command(id).unwrap();
        assert_eq!(command.state(), CommandState::Blocked);
        assert!(command.dispatch_key().is_none());
        assert!(command
            .block_reason()
            .unwrap()
            .contains("no exact world executor"));
    }

    #[test]
    fn permit_is_registered_once_before_project_creation() {
        let (mut commands, id) = approved_clear_flatten(
            IVec3::new(0, 10, 0),
            IVec3::new(1, 11, 1),
            CommandRecipients::Selected(vec![1]),
        );
        let plan = ExactBotCommandPlan::ClearFlatten;
        let mut executor = BotCommandExecutor::default();

        let permit = commands.claim_dispatch(id).unwrap().unwrap();
        executor.insert_claim(permit, plan);

        assert!(commands.claim_dispatch(id).unwrap().is_none());
        assert_eq!(executor.jobs.len(), 1);
        assert!(executor.jobs.contains_key(&id));
    }

    #[test]
    fn world_unload_retires_pending_permit_before_persistence() {
        let (mut commands, id) = approved_clear_flatten(
            IVec3::new(0, 10, 0),
            IVec3::new(1, 11, 1),
            CommandRecipients::Selected(vec![1]),
        );
        let permit = commands.claim_dispatch(id).unwrap().unwrap();
        let mut executor = BotCommandExecutor::default();
        executor.insert_claim(permit, ExactBotCommandPlan::ClearFlatten);
        let mut brain = FriendlyWorldBrain::default();

        retire_all_bot_command_jobs(&mut commands, &mut executor, &mut brain, "world unloaded");

        assert_eq!(
            commands.command(id).unwrap().state(),
            CommandState::Cancelled
        );
        assert!(executor.jobs.is_empty());
        assert!(executor.project_to_command.is_empty());
    }

    #[test]
    fn pause_transition_preserves_executor_job_until_real_world_unload() {
        let (mut commands, id) = approved_clear_flatten(
            IVec3::new(0, 10, 0),
            IVec3::new(1, 11, 1),
            CommandRecipients::Selected(vec![1]),
        );
        let permit = commands.claim_dispatch(id).unwrap().unwrap();
        let mut executor = BotCommandExecutor::default();
        executor.insert_claim(permit, ExactBotCommandPlan::ClearFlatten);

        let mut app = App::new();
        app.add_plugins(bevy::state::app::StatesPlugin)
            .insert_state(GameState::InGame)
            .insert_resource(commands)
            .insert_resource(executor)
            .insert_resource(FriendlyWorldBrain::default())
            .add_systems(
                OnEnter(GameState::MainMenu),
                cancel_bot_command_jobs_on_world_unload,
            );
        app.update();

        app.world_mut()
            .resource_mut::<NextState<GameState>>()
            .set(GameState::Paused);
        app.update();

        assert_eq!(
            app.world()
                .resource::<BotCommandStateMachine>()
                .command(id)
                .unwrap()
                .state(),
            CommandState::Running
        );
        assert!(app
            .world()
            .resource::<BotCommandExecutor>()
            .jobs
            .contains_key(&id));

        app.world_mut()
            .resource_mut::<NextState<GameState>>()
            .set(GameState::MainMenu);
        app.update();

        assert_eq!(
            app.world()
                .resource::<BotCommandStateMachine>()
                .command(id)
                .unwrap()
                .state(),
            CommandState::Cancelled
        );
        assert!(app.world().resource::<BotCommandExecutor>().jobs.is_empty());
    }

    #[test]
    fn progress_counts_unique_chunks_and_actual_edits() {
        let (mut commands, id) = approved_clear_flatten(
            IVec3::new(0, 10, 0),
            IVec3::new(31, 10, 0),
            CommandRecipients::Selected(vec![1]),
        );
        let permit = commands.claim_dispatch(id).unwrap().unwrap();
        let mut executor = BotCommandExecutor::default();
        executor.insert_claim(permit, ExactBotCommandPlan::ClearFlatten);
        executor.attach_project(id, 44);

        executor.record_project_progress(
            44,
            3,
            [
                ChunkPos::new(0, 0, 0),
                ChunkPos::new(0, 0, 0),
                ChunkPos::new(1, 0, 0),
            ],
        );
        executor.record_project_progress(44, 2, [ChunkPos::new(1, 0, 0)]);

        let job = executor.jobs.get(&id).unwrap();
        assert_eq!(job.applied_voxel_edits, 5);
        assert_eq!(job.touched_chunks.len(), 2);
    }

    #[test]
    fn invalid_world_recipient_blocks_before_permit_claim_or_project_mutation() {
        let (commands, id) = approved_clear_flatten(
            IVec3::new(0, 80, 0),
            IVec3::new(1, 81, 1),
            CommandRecipients::Selected(vec![999]),
        );
        let mut world = VoxelWorld::new();
        world.loaded_column_counts.insert((0, 0), 1);
        let mut brain = FriendlyWorldBrain::default();
        brain.save.autonomy.bots_active = true;
        let next_project_id = brain.save.next_project_id;
        let next_crew_id = brain.save.next_crew_id;

        let mut app = App::new();
        app.insert_resource(commands)
            .insert_resource(BotCommandExecutor::default())
            .insert_resource(brain)
            .insert_resource(world)
            .add_systems(Update, dispatch_authorized_bot_commands);
        app.update();

        let commands = app.world().resource::<BotCommandStateMachine>();
        let command = commands.command(id).unwrap();
        assert_eq!(command.state(), CommandState::Blocked);
        assert!(command.dispatch_key().is_none());
        assert!(command.block_reason().unwrap().contains("does not exist"));
        let brain = app.world().resource::<FriendlyWorldBrain>();
        assert_eq!(brain.save.next_project_id, next_project_id);
        assert_eq!(brain.save.next_crew_id, next_crew_id);
        assert!(brain.save.projects.is_empty());
        assert!(brain.save.crews.is_empty());
    }
}
