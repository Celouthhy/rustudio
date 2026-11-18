//! Keep plugin work on the main thread when the window stops receiving redraws.
//! On macOS, App Nap can still delay event-loop wakeups while the app is hidden.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Instant;

use eframe::{egui, App, CreationContext, EframeWinitApplication, NativeOptions, UserEvent};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

use crate::app::RustudioApp;

// Both eframe and the deadline callback access the app on the event-loop thread.
struct SharedApp(Rc<RefCell<RustudioApp>>);

impl App for SharedApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.0.borrow_mut().raw_input_hook(ctx, raw);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.0.borrow_mut().logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.0.borrow_mut().ui(ui, frame);
    }

    fn on_exit(&mut self) {
        self.0.borrow_mut().on_exit();
    }
}

pub fn run(
    name: &str,
    mut options: NativeOptions,
    create: impl FnOnce(
        &CreationContext<'_>,
    ) -> Result<Box<RustudioApp>, Box<dyn std::error::Error + Send + Sync>>,
) -> eframe::Result {
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    if let Some(hook) = options.event_loop_builder.take() {
        hook(&mut builder);
    }
    let event_loop = builder.build()?;
    let app_handle = RefCell::new(None);
    let initialized = Cell::new(false);
    let eframe = eframe::create_native(
        name,
        options,
        Box::new(|cc| {
            let app = create(cc)?;
            initialized.set(true);
            let app = Rc::new(RefCell::new(*app));
            *app_handle.borrow_mut() = Some(Rc::downgrade(&app));
            Ok(Box::new(SharedApp(app)))
        }),
        &event_loop,
    );
    event_loop.run_app(&mut NativeLoop {
        eframe,
        app_handle: &app_handle,
    })?;
    // create_native logs its initialization error before exiting the loop.
    // Preserve a failing exit status even though it only exposes winit's result.
    if !initialized.get() {
        return Err(eframe::Error::AppCreation(
            "eframe failed to initialize the editor".into(),
        ));
    }
    Ok(())
}

struct NativeLoop<'a> {
    eframe: EframeWinitApplication<'a>,
    app_handle: &'a RefCell<Option<Weak<RefCell<RustudioApp>>>>,
}

impl ApplicationHandler<UserEvent> for NativeLoop<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.resumed(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.exiting(event_loop);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.eframe.new_events(event_loop, cause);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.eframe.window_event(event_loop, id, event);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        self.eframe.device_event(event_loop, id, event);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.eframe.user_event(event_loop, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.about_to_wait(event_loop);
        if event_loop.exiting() {
            return;
        }
        // A redraw dispatched in this iteration has already ticked plugins.
        // If the compositor paused redraws, work from the deadline directly.
        let Some(app) = self.app_handle.borrow().as_ref().and_then(Weak::upgrade) else {
            return;
        };
        let deadline = app.borrow_mut().tick_plugins_between_frames(Instant::now());
        if let Some(deadline) = deadline {
            event_loop.set_control_flow(with_plugin_deadline(event_loop.control_flow(), deadline));
        }
    }
}

fn with_plugin_deadline(flow: ControlFlow, deadline: Instant) -> ControlFlow {
    match flow {
        ControlFlow::Wait => ControlFlow::WaitUntil(deadline),
        ControlFlow::WaitUntil(repaint) => ControlFlow::WaitUntil(deadline.min(repaint)),
        ControlFlow::Poll => ControlFlow::Poll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn plugin_deadline_wakes_a_sleeping_event_loop_without_delaying_redraws() {
        let now = Instant::now();
        let deadline = now + Duration::from_millis(100);
        let later = deadline + Duration::from_millis(1);
        assert_eq!(
            with_plugin_deadline(ControlFlow::Wait, deadline),
            ControlFlow::WaitUntil(deadline)
        );
        assert_eq!(
            with_plugin_deadline(ControlFlow::WaitUntil(later), deadline),
            ControlFlow::WaitUntil(deadline)
        );
        assert_eq!(
            with_plugin_deadline(ControlFlow::WaitUntil(now), deadline),
            ControlFlow::WaitUntil(now)
        );
        assert_eq!(
            with_plugin_deadline(ControlFlow::Poll, deadline),
            ControlFlow::Poll
        );
    }
}
