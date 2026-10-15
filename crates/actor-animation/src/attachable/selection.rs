//! Owner conditions select an item definition before its animation state is created.
use super::*;
use crate::evaluation::Evaluator;

impl AttachablesRuntime {
    /// Tests an authored definition's item condition; absent rigs remain unresolved.
    pub fn matches_owner(
        &mut self,
        identifier: &str,
        owner: &dyn AnimatedActor,
        rig: &ActorRigSnapshot<'_>,
        input: AttachableAnimationInput<'_>,
    ) -> Option<bool> {
        let binding = self.assets.attachable_rig_binding(identifier)?;
        let Some(condition) = self.assets.rig_bindings()[binding].item_condition else {
            return Some(true);
        };
        let frame_alpha = if input.frame_alpha.is_finite() {
            input.frame_alpha.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let context = owner_context(rig, input);
        let tick_input = ActorTickInput {
            position: owner.position(),
            velocity: owner.velocity(),
            on_ground: owner.on_ground(),
            ..rig.animation_variables.input().unwrap_or_default()
        };
        self.selection_variables
            .reset(owner.runtime_id() ^ owner.spawn_revision().rotate_left(32));
        tick::apply_engine_variables(
            &self.layout.engine,
            &mut self.selection_variables,
            owner,
            &context,
            &tick_input,
            &motion::MotionState::default(),
        );
        rig.animation_variables
            .copy_to(&self.assets, &self.layout, &mut self.selection_variables);
        for (name, value) in input.owner_variables {
            self.selection_variables
                .set(self.layout.named_slot(&self.assets, name), *value);
        }
        let mut world_left = MAX_MOLANG_OPS_PER_ACTOR_TICK;
        let mut budget = EvalBudget {
            actor_left: MAX_MOLANG_OPS_PER_ACTOR_TICK,
            world_left: &mut world_left,
            work_left: MAX_RUNTIME_POSE_WORK_PER_ACTOR_TICK,
            transitions_left: MAX_CONTROLLER_TRANSITIONS_PER_TICK,
            used: 0,
            stack: std::mem::take(&mut self.selection_stack),
            static_draw: None,
        };
        let evaluator = Evaluator {
            assets: &self.assets,
            pack: true,
            layout: &self.layout,
            program: None,
            actor: owner,
            input: &tick_input,
            context: &context,
            anim_tick: rig.completed_tick,
            anim_time: None,
            swell_amount: None,
            presentation_alpha: Some(frame_alpha),
            query_history: None,
            life_tick: rig.animation_variables.life_tick(),
            finished: (false, false),
            state_time: 0.0,
            bones: &[],
            bone_names: &[],
        };
        let matches = evaluator
            .run(
                condition as usize,
                &mut self.selection_variables,
                0.0,
                &mut budget,
            )
            .is_ok_and(|value| value.truthy());
        self.selection_stack = budget.stack;
        Some(matches)
    }
}

/// Builds the equipment owner's complete query context for selection and animation.
pub(super) fn owner_context(
    owner_rig: &ActorRigSnapshot<'_>,
    input: AttachableAnimationInput<'_>,
) -> ActorTickContext {
    let frame_alpha = if input.frame_alpha.is_finite() {
        input.frame_alpha.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let query_context = AttachableQueryContext {
        first_person: input.first_person,
        off_hand: input.off_hand,
        worn: input.worn,
        worn_slot: input.worn_slot,
        is_paperdoll: input.is_paperdoll,
        frame_alpha,
        delta_seconds: input
            .delta_seconds
            .filter(|delta| delta.is_finite() && *delta >= 0.0),
        animation_frame: input.animation_frame,
        use_elapsed_ticks: input.use_elapsed_ticks,
        max_use_ticks: input.max_use_ticks,
        owner_life_tick: owner_rig.animation_variables.life_tick(),
    };
    let (main_hand_kinetic, main_hand_swing_seconds) = owner_rig.animation_variables.item_timings();
    let owner_context = owner_rig.animation_variables.owner_context();
    let context = ActorTickContext {
        // Attachables use render time, independently of the owner's tick batching.
        animation_elapsed_ticks: None,
        main_hand_kinetic,
        main_hand_swing_seconds,
        main_hand_is_spear: owner_rig.animation_variables.is_spear(),
        is_local_first_person: input.first_person,
        is_local_player: input.is_local_player
            || input.first_person
            || owner_context.is_some_and(|owner| owner.is_local_player),
        hand_charged: input.hand_charged,
        main_hand_max_use_ticks: input.max_use_ticks,
        attachable: Some(query_context),
        main_hand: input.owner_main_hand.map(Arc::from),
        off_hand: input.owner_off_hand.map(Arc::from),
        ..owner_context.cloned().unwrap_or_default()
    };
    context
}
