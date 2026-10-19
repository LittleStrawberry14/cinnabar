//! The Shader Pack Settings screen over Video settings: presses queue for the app, which owns
//! the pack's options and publishes the screen back.

use std::sync::Arc;

use launcher::menu::{MenuScreen, RendererChoice};
use launcher::shader_options::{Action, View};

use super::MenuRuntime;

/// Queued commands and the published screen; the screen is open while `view` is set.
#[derive(Debug, Default)]
pub(super) struct ShaderOptions {
    actions: Vec<Action>,
    view: Option<Arc<View>>,
}

impl MenuRuntime {
    /// Queues a press. [`Action::Open`] opens the screen for the selected pack, showing it as
    /// loading until the app publishes it; other presses need the screen open.
    pub(super) fn activate_shader_options(&mut self, action: Action) {
        if !render_model::SHADER_PACKS_ENABLED {
            return;
        }
        if action == Action::Open {
            let Some(pack) = self
                .shader_pack
                .as_deref()
                .filter(|_| matches!(self.renderer_choice(), Some(RendererChoice::ShaderPack(_))))
            else {
                return;
            };
            self.shader_options.view = Some(Arc::new(View::loading(pack)));
        } else if self.shader_options.view.is_none() {
            return;
        }
        self.shader_options.actions.push(action);
    }

    /// Back on Settings while the screen is open leaves its sub-screen or closes it.
    pub(super) fn shader_options_back(&mut self) -> bool {
        let open = self.shader_options.view.is_some()
            && self.dialog.is_none()
            && self.screen == MenuScreen::Settings;
        if open {
            self.shader_options.actions.push(Action::Back);
        }
        open
    }

    /// Leaving the Video section closes the screen, applying its changes as Iris does.
    pub(super) fn leave_shader_options(&mut self) {
        if self.shader_options.view.is_some() {
            self.shader_options.actions.push(Action::Done);
        }
    }

    /// The commands queued since the last call, oldest first.
    pub fn take_shader_option_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.shader_options.actions)
    }

    /// Publishes the open screen, or closes it with `None`.
    pub fn sync_shader_options(&mut self, view: Option<View>) {
        self.shader_options.view = view.map(Arc::new);
    }

    /// The screen for [`launcher::menu::MenuView`].
    pub(super) fn shader_options_view(&self) -> Option<Arc<View>> {
        self.shader_options.view.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher::menu::MenuAction;

    fn menu() -> MenuRuntime {
        let mut menu = MenuRuntime::new(true, 2, "Steve".to_owned());
        menu.sync_shader_packs(vec!["Dusk.zip".to_owned()]);
        menu
    }

    #[test]
    fn the_screen_opens_only_for_a_selected_pack() {
        let mut menu = menu();
        menu.activate(MenuAction::ShaderOptions(Action::Open));
        menu.activate(MenuAction::ShaderOptions(Action::Apply));
        assert!(menu.take_shader_option_actions().is_empty());
        assert_eq!(menu.view().shader_options, None);
        menu.activate(MenuAction::SelectRenderer(RendererChoice::ShaderPack(0)));
        menu.activate(MenuAction::ShaderOptions(Action::Open));
        if !render_model::SHADER_PACKS_ENABLED {
            assert!(menu.take_shader_option_actions().is_empty());
            return;
        }
        assert_eq!(
            menu.view().shader_options.as_deref(),
            Some(&View::loading("Dusk.zip"))
        );
        menu.activate(MenuAction::ShaderOptions(Action::Apply));
        assert_eq!(
            menu.take_shader_option_actions(),
            [Action::Open, Action::Apply]
        );
        menu.sync_shader_options(None);
        menu.activate(MenuAction::ShaderOptions(Action::Apply));
        assert!(menu.take_shader_option_actions().is_empty());
    }

    #[test]
    fn back_and_leaving_video_go_to_the_open_screen() {
        if !render_model::SHADER_PACKS_ENABLED {
            return;
        }
        let mut menu = menu();
        menu.activate(MenuAction::Navigate(MenuScreen::Settings));
        menu.activate(MenuAction::SelectRenderer(RendererChoice::ShaderPack(0)));
        menu.activate(MenuAction::ShaderOptions(Action::Open));
        menu.take_shader_option_actions();
        menu.activate(MenuAction::AddBack);
        assert_eq!(
            menu.screen(),
            MenuScreen::Settings,
            "Back stays on Settings"
        );
        menu.activate(MenuAction::SettingsSection(0));
        assert_eq!(
            menu.take_shader_option_actions(),
            [Action::Back, Action::Done]
        );
    }
}
