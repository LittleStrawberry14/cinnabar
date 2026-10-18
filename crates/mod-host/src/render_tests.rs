use super::*;
use crate::ModGrants;
use wit::Host as _;

fn state() -> State {
    let grants = ModGrants {
        render: true,
        ..Default::default()
    };
    State::new(grants, String::new(), Default::default())
}

#[test]
fn unchanged_pass_updates_stage_nothing() {
    let mut state = state();
    let source = "fn effect(uv: vec2<f32>) -> vec3<f32> { return scene(uv); }";
    let spec = wit::PassSpec {
        name: "grade".into(),
        order: 0,
        source: source.into(),
        depth: false,
    };
    state.register_pass(spec).unwrap().unwrap();
    state
        .update_pass("grade".into(), true, vec![0.5])
        .unwrap()
        .unwrap();
    state.render.commit();
    state.render.begin_frame();
    state
        .update_pass("grade".into(), true, vec![0.5])
        .unwrap()
        .unwrap();
    assert!(
        state.render.staged_passes.is_none(),
        "a no-op update copies no passes"
    );
    state
        .update_pass("grade".into(), true, vec![0.75])
        .unwrap()
        .unwrap();
    assert!(state.render.staged_passes.is_some());
    assert!(
        state
            .update_pass("missing".into(), true, vec![])
            .unwrap()
            .is_err()
    );
}

/// Additive line drawing preserves legacy ribbons and rejects overflow atomically.
#[test]
fn solid_lines_share_the_legacy_ribbon_budget() {
    let mut state = state();
    let ribbon = wit::Ribbon {
        points: vec![
            wit::Vector3 {
                x: 0.,
                y: 0.,
                z: 0.,
            },
            wit::Vector3 {
                x: 1.,
                y: 1.,
                z: 1.,
            },
        ],
        width: 0.01,
        color: wit::Rgba {
            r: 1.,
            g: 1.,
            b: 1.,
            a: 1.,
        },
    };
    state
        .draw(wit::Primitives {
            decals: vec![],
            ribbons: vec![ribbon.clone(); mod_api::MAX_RENDER_RIBBONS - 1],
            beams: vec![],
            billboards: vec![],
        })
        .unwrap()
        .unwrap();
    let line = wit::Line {
        points: ribbon.points.clone(),
        width: 1.,
        color: ribbon.color,
        pattern: wit::LinePattern::Solid,
    };
    state.draw_lines(vec![line.clone()]).unwrap().unwrap();
    assert!(state.render.staged_primitives.ribbons[0].taper);
    assert!(!state.render.staged_primitives.ribbons.last().unwrap().taper);
    assert!(state.draw_lines(vec![line]).unwrap().is_err());
    assert_eq!(
        state.render.staged_primitives.ribbons.len(),
        mod_api::MAX_RENDER_RIBBONS
    );
}

#[test]
fn legacy_post_imports_cannot_bypass_remote_policy() {
    let mut state = state();
    state.drawing_policy = Some(mod_api::drawing::DrawingSession {
        connection: 1,
        permissions: mod_api::drawing::DrawingPermissions {
            world_draw: true,
            ..Default::default()
        },
        ..Default::default()
    });
    assert!(
        state
            .register_pass(wit::PassSpec {
                name: "grade".into(),
                order: 0,
                source: "fn effect(uv: vec2<f32>) -> vec3<f32> { return scene(uv); }".into(),
                depth: false,
            })
            .unwrap()
            .is_err()
    );
    assert!(
        state
            .update_pass("grade".into(), true, vec![])
            .unwrap()
            .is_err()
    );
    assert!(state.remove_pass("grade".into()).unwrap().is_err());
    state.render.commit();
    assert!(state.render.output().passes.is_empty());
}

/// Overlay drawing never inherits authority from ordinary geometry or post effects.
#[test]
fn overlay_lines_require_consent_and_session_authority_and_clear_each_callback() {
    let mut state = state();
    let line = wit::Line {
        points: vec![
            wit::Vector3 {
                x: 0.,
                y: 0.,
                z: 0.,
            },
            wit::Vector3 {
                x: 1.,
                y: 1.,
                z: 1.,
            },
        ],
        width: 2.,
        color: wit::Rgba {
            r: 1.,
            g: 0.5,
            b: 0.2,
            a: 1.,
        },
        pattern: wit::LinePattern::Solid,
    };
    assert!(
        state
            .draw_overlay_lines(vec![line.clone()])
            .unwrap()
            .is_err()
    );
    state.grants.render_through_walls = true;
    state.drawing_policy = Some(mod_api::drawing::DrawingSession::default());
    assert!(
        state
            .draw_overlay_lines(vec![line.clone()])
            .unwrap()
            .is_err()
    );
    state
        .drawing_policy
        .as_mut()
        .unwrap()
        .permissions
        .render_through_walls = true;
    state
        .draw_overlay_lines(vec![line.clone()])
        .unwrap()
        .unwrap();
    assert!(state.render.staged_primitives.ribbons[0].through_world);
    state
        .draw_lines(vec![line.clone(); mod_api::MAX_RENDER_RIBBONS - 1])
        .unwrap()
        .unwrap();
    assert!(state.draw_overlay_lines(vec![line]).unwrap().is_err());
    state.render.commit();
    assert_eq!(
        state.render.output().primitives.ribbons.len(),
        mod_api::MAX_RENDER_RIBBONS
    );
    state.render.begin_frame();
    state.render.commit();
    assert!(state.render.output().primitives.is_empty());
}

/// Compact boxes obey layer authority and validation before mutating staged output.
#[test]
fn wire_boxes_validate_bounds_and_preserve_ordinary_drawing_authority() {
    let mut state = state();
    let mut b = wit::WireBox {
        min: wit::Vector3 {
            x: 0.,
            y: 0.,
            z: 0.,
        },
        max: wit::Vector3 {
            x: 1.,
            y: 1.,
            z: 1.,
        },
        width: 2.,
        color: wit::Rgba {
            r: 1.,
            g: 1.,
            b: 1.,
            a: 1.,
        },
    };
    assert!(
        state
            .draw_wire_boxes(vec![b.clone()], true)
            .unwrap()
            .is_err()
    );
    state
        .draw_wire_boxes(vec![b.clone()], false)
        .unwrap()
        .unwrap();
    assert!(!state.render.staged_primitives.ribbons[0].through_world);
    state.grants.render_through_walls = true;
    state
        .draw_wire_boxes(vec![b.clone()], true)
        .unwrap()
        .unwrap();
    assert!(state.render.staged_primitives.ribbons[1].through_world);
    b.max.x = -1.;
    assert!(
        state
            .draw_wire_boxes(vec![b.clone()], true)
            .unwrap()
            .is_err()
    );
    b.max.x = f32::INFINITY;
    assert!(state.draw_wire_boxes(vec![b], true).unwrap().is_err());
    assert_eq!(state.render.staged_primitives.ribbons.len(), 2);
}
