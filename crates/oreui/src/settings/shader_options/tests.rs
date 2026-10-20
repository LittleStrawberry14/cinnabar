//! The Shader Pack Settings grid stays where the player scrolled it, as Iris keeps it.

use std::{collections::HashMap, sync::Arc};

use launcher::menu::{MenuAction, MenuView};
use launcher::shader_options::{Action, Control, Element, Input, Status, View};
use ui::{DpiScale, SafeArea, TextLayoutCache};
use ui_engine::{
    engine::screen::{PaintFrame, Screen},
    menu_scroll::MenuScrolls,
    test_support::fixture_font,
    text_metrics::TextMetrics,
};

use crate::{
    OreUiScreen,
    menu_screen::{MenuGeometry, MenuInput},
    test_support::settings_view,
};

const SIZE: [f32; 2] = [1280.0, 720.0];

/// Paints the menu as the client does, revealing the focused control through `scrolls`.
fn paint(screen: &mut OreUiScreen, view: &MenuView, scrolls: &mut MenuScrolls<MenuAction>) {
    let font = fixture_font();
    let mut layouts = TextLayoutCache::new(64, 1024 * 1024);
    let (mut nodes, mut next) = (Vec::new(), 1);
    let artwork = HashMap::new();
    let mut frame = PaintFrame {
        layouts: &mut layouts,
        font: &font,
        metrics: TextMetrics::for_viewport(
            SIZE.map(|value| value as u32),
            DpiScale::new(1.0).unwrap(),
            Some(2),
        ),
        solid_page: 0,
        safe_area: SafeArea::ZERO,
        content: SIZE,
        translate: &|_| None,
        language: [0; 3],
        now: 0.0,
        nodes: &mut nodes,
        next: &mut next,
        renderers: &mut [],
    };
    screen
        .paint(
            MenuInput {
                view,
                size: SIZE,
                portrait: None,
                translate: &|_| None,
                artwork: &artwork,
                title_artwork: None,
                scrolls,
                geometry: MenuGeometry::default(),
            },
            &mut frame,
        )
        .unwrap();
}

/// How far the option grid is scrolled.
fn grid_offset(scrolls: &MenuScrolls<MenuAction>) -> f32 {
    scrolls
        .offsets()
        .iter()
        .find_map(|(key, offset)| key.contains("/shader_options/").then_some(*offset))
        .unwrap_or(0.0)
}

#[test]
fn footer_buttons_leave_the_grid_scrolled_where_it_was() {
    let mut view = settings_view("video_forced_index");
    view.shader_options = Some(Arc::new(View {
        pack: "Fixture".into(),
        title: "Lighting".into(),
        screen: 1,
        nested: true,
        columns: 2,
        elements: (0..60)
            .map(|index| Element {
                label: format!("Screen {index}"),
                comment: None,
                control: Control::Link,
                modified: false,
            })
            .collect(),
        changed: true,
        status: Status::Ready,
    }));
    let mut screen = OreUiScreen::default();
    let mut scrolls = MenuScrolls::default();
    paint(&mut screen, &view, &mut scrolls);
    view.focused_action = Some(MenuAction::ShaderOptions(Action::Element {
        screen: 1,
        index: 30,
        input: Input::Next,
    }));
    paint(&mut screen, &view, &mut scrolls);
    let scrolled = grid_offset(&scrolls);
    assert!(scrolled > 0.0, "focusing a lower cell scrolls the grid");
    for action in [
        Action::Apply,
        Action::Reset,
        Action::Cancel,
        Action::Done,
        Action::Back,
    ] {
        view.focused_action = Some(MenuAction::ShaderOptions(action));
        paint(&mut screen, &view, &mut scrolls);
        assert_eq!(grid_offset(&scrolls), scrolled, "{action:?}");
    }
}
