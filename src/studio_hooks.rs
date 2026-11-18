//! Installs Studio's plugin API, settings, gap telemetry and Output limit.
//! Call [`install`] before constructing a Game or creating an engine runtime
//! for Studio. Engine-only hosts use the built-in domain tables.

use std::sync::Once;

/// Installs every studio hook in the engine (idempotent).
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // The plugin API domain (src/plugin_host/api.rs).
        crate::plugin_host::api::install_domain();
        crate::studio_settings::install_hooks();
        rustudio_core::telemetry::install_engine_tables(
            crate::engine_tables::ENGINE_READS,
            crate::engine_tables::ENGINE_FIRES,
        );
        crate::game::install_output_limit(|| crate::studio_settings::studio("Maximum Output Lines", 5000.0));
    });
}

/// Creates an engine runtime with Studio's services installed.
pub fn new_runtime() -> Result<Runtime, String> {
    install();
    Runtime::new()
}

use std::collections::{HashMap, HashSet};

use rbx_dom_weak::types::Ref;
use rustudio_core::{doc::Document, log::LogLine};
use rustudio_engine::{gui::GuiBox, runtime::{RunMode, Runtime, Side}};

use crate::{
    app::{Editor, Harness},
    explorer::ExplorerState,
    game::GameHost,
    plugin_host::PluginHost,
    viewport::GizmoDrag,
};

/// Borrows the Studio state a Game transition can affect.
struct StudioGameHost<'a> {
    plugins: &'a mut PluginHost,
    explorer: &'a mut ExplorerState,
    drag: &'a mut Option<GizmoDrag>,
    harness: &'a mut Harness,
}

impl GameHost for StudioGameHost<'_> {
    fn document_changed(&mut self, doc: &Document) {
        self.explorer.expand_defaults(doc);
        *self.drag = None;
    }

    fn load_plugins(&mut self, runtime: &Runtime) {
        self.plugins.load_into_play(runtime);
    }

    fn unload_plugins(&mut self, runtime: &Runtime) {
        self.plugins.unload_from_play(runtime);
    }

    fn plugin_pins(&self, lua: &mlua::Lua) -> HashSet<Ref> {
        crate::plugin_host::vm::pinned(lua)
    }

    fn keep_play_layout(&self, old: &HashMap<Ref, GuiBox>, new: &mut HashMap<Ref, GuiBox>) {
        crate::plugin_host::widgets_ui::keep_play_layout(old, new);
    }

    fn end_test(&mut self, value: String, log: &[LogLine]) {
        if self.harness.run.ends_on_end_test() {
            self.harness.run.end_value = Some(value);
        } else {
            rustudio_engine::runtime::domains::testing::report_test_run(&value, log);
        }
    }
}

impl Editor {
    fn with_game_host(&mut self, play: impl FnOnce(&mut crate::game::Game, &mut dyn GameHost)) {
        let mut host = StudioGameHost {
            plugins: &mut self.plugins,
            explorer: &mut self.explorer,
            drag: &mut self.drag,
            harness: &mut self.harness,
        };
        play(&mut self.game, &mut host);
        self.flush_output_changes();
    }

    pub fn start_play(&mut self, mode: RunMode) {
        if self.game.is_playing() {
            return;
        }
        if self.defer_play_until_loaded(mode) {
            return;
        }
        let clear_output = self.harness.plain()
            && rustudio_core::rustudio_settings::studio_flag("Studio", "Clear Output On Start", true);
        let live_ids = self.live_team_create_ids();
        self.game.keep_game_settings = self.harness.keeps_game_settings();
        self.with_game_host(|game, host| game.start_play(mode, host, clear_output, live_ids));
    }

    pub fn stop_play(&mut self) {
        self.with_game_host(|game, host| game.stop_play(host));
    }

    pub fn show_play_side(&mut self, side: Side) {
        self.with_game_host(|game, host| game.show_play_side(side, host));
    }

    pub fn play_tick(&mut self, ctx: &egui::Context, dt: f32) {
        let (no_ui, shift) = (self.harness.run.no_ui, self.harness.run.input.shift);
        self.game.keep_game_settings = self.harness.keeps_game_settings();
        self.with_game_host(|game, host| game.play_tick(ctx, dt, host, no_ui, shift));
    }
}
