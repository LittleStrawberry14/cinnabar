//! Hosts the Shader Pack Settings screen: reads the selected pack and its Iris option file on
//! a worker, runs the screen's commands through [`Editor`], saves changed values beside the
//! pack and reloads it, as Iris applies them.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};

use bevy::prelude::*;
use launcher::shader_options::{Action, Editor, View};
use shaderpack::{OptionSession, OptionValues, ShaderPack};

use super::shader_pack::{SelectedPack, selected_pack};
use super::{MenuRuntime, RenderModeConfig, RuntimeSettings};

/// The open screen, if any.
#[derive(Resource, Default)]
pub(super) struct ShaderOptionsHost {
    open: Option<Open>,
}

/// One open screen and where its pack's values are saved.
struct Open {
    name: String,
    file: Option<PathBuf>,
    state: State,
}

enum State {
    Loading(Mutex<mpsc::Receiver<Result<OptionSession, String>>>),
    Ready(Editor),
    Failed(String),
}

impl Open {
    /// Starts reading `pack` on a worker.
    fn start(pack: SelectedPack) -> Open {
        let (send, receive) = mpsc::channel();
        let (path, file) = (pack.path, pack.options.clone());
        let spawned = std::thread::Builder::new()
            .name("shader pack options".into())
            .spawn(move || {
                let _ = send.send(read_session(&path, file.as_deref()));
            });
        let state = match spawned {
            Ok(_) => State::Loading(Mutex::new(receive)),
            Err(error) => State::Failed(error.to_string()),
        };
        Open {
            name: pack.name,
            file: pack.options,
            state,
        }
    }

    /// The screen to show.
    fn view(&self) -> View {
        match &self.state {
            State::Loading(_) => View::loading(&self.name),
            State::Ready(editor) => editor.view(),
            State::Failed(reason) => View::failed(&self.name, reason.clone()),
        }
    }

    /// Takes the worker's result once it arrives; `true` when the state changed.
    fn poll(&mut self) -> bool {
        let State::Loading(receive) = &mut self.state else {
            return false;
        };
        let received = receive
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .try_recv();
        self.state = match received {
            Ok(Ok(session)) => State::Ready(Editor::new(session, &self.name)),
            Ok(Err(reason)) => State::Failed(reason),
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                State::Failed("the options reader stopped".to_owned())
            }
        };
        true
    }
}

/// Opens the pack at `path` with the values saved in `file`.
fn read_session(path: &Path, file: Option<&Path>) -> Result<OptionSession, String> {
    let pack = ShaderPack::open(path).map_err(|error| error.to_string())?;
    let saved = match file {
        Some(file) => shaderpack::read_option_file(file)
            .map_err(|error| format!("{}: {error}", file.display()))?,
        None => OptionValues::new(),
    };
    Ok(OptionSession::new(&pack, &saved))
}

/// Saves `values` as the pack's option file; the reload reads them back.
fn save(file: Option<&Path>, editor: &Editor, values: &OptionValues) {
    let Some(file) = file else {
        return;
    };
    if let Err(error) = shaderpack::write_option_file(file, editor.session().options(), values) {
        warn!(
            file = %file.display(),
            "shader pack options could not be saved: {error}"
        );
    }
}

/// Runs the screen's commands, saving and reloading the pack when values are applied, and
/// publishes the screen whenever it changes.
pub(super) fn drive_shader_options(
    config: Res<RenderModeConfig>,
    settings: Res<RuntimeSettings>,
    menu: Option<ResMut<MenuRuntime>>,
    mut host: ResMut<ShaderOptionsHost>,
) {
    let Some(mut menu) = menu else {
        return;
    };
    let mut changed = false;
    for action in menu.take_shader_option_actions() {
        changed = true;
        if action == Action::Open {
            host.open =
                selected_pack(&config, &settings.user_settings_update().1.video).map(Open::start);
            continue;
        }
        let Some(open) = host.open.as_mut() else {
            continue;
        };
        let close = match &mut open.state {
            State::Ready(editor) => {
                let outcome = editor.handle(action);
                if let Some(values) = &outcome.save {
                    save(open.file.as_deref(), editor, values);
                    menu.request_shader_pack_reload();
                }
                outcome.close
            }
            State::Loading(_) | State::Failed(_) => {
                matches!(action, Action::Back | Action::Cancel | Action::Done)
            }
        };
        if close {
            host.open = None;
        }
    }
    if let Some(open) = host.open.as_mut() {
        changed |= open.poll();
    }
    if changed {
        menu.sync_shader_options(host.open.as_ref().map(Open::view));
    }
}
