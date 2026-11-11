use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{Error, Result};
use hudhook::{BeforeWndProc, ImguiRenderLoop, RenderContext};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use imgui::*;

use crate::{Core, Game, InputBlocker, InputFlags, overlay::Overlay, utils::PopupModalExt};

/// A wrapper around the rest of the mod's UI that doesn't expect any state to
/// exist. This allows the full [Overlay] to assume that its [Core] exists while
/// still using Hudhook and ImGui to surface fatal errors that may occur during
/// initialization.
pub(crate) struct ErrorDisplay<G: Game> {
    /// The struct that's used to block and unblock input going to the game.
    input_blocker: G::InputBlocker,

    /// Whether the main HUD is currently visible. This is atomic because the
    /// window-procedure callback and render loop may run on different threads.
    hud_visible: AtomicBool,

    /// The main overlay if it managed to initialize correctly, or [None]
    /// otherwise.
    overlay: Option<Overlay<G>>,

    /// The core game logic. Used to extract fatal errors to display to the
    /// user.
    core: Option<Arc<Mutex<G::Core>>>,

    /// A fatal error to display. Once set, this can't be changed, even if other
    /// fatal errors are detected later.
    error: Option<Error>,

    /// Whether to display the full error information or just the summary.
    show_full_error: bool,
}

impl<G: Game> ErrorDisplay<G> {
    /// Creates a new [ErrorDisplay].
    pub fn new(core: Result<Arc<Mutex<G::Core>>>, input_blocker: G::InputBlocker) -> Self {
        match core {
            Ok(core) => Self {
                input_blocker,
                hud_visible: AtomicBool::new(true),
                overlay: Some(Overlay::new()),
                core: Some(core),
                error: None,
                show_full_error: false,
            },
            Err(error) => Self {
                input_blocker,
                hud_visible: AtomicBool::new(true),
                overlay: None,
                core: None,
                error: Some(error),
                show_full_error: false,
            },
        }
    }

    /// Displays a fatal error to the user if one is set.
    fn render_error(&mut self, ui: &mut Ui) {
        let Some(error) = &self.error else { return };

        // Make sure the cursor is visible even if the player is loaded into a
        // save with the menu closed.
        //
        // Safety: This is only ever run on the main thread.
        unsafe {
            G::force_cursor_visible();
        }

        unsafe {
            imgui_sys::igSetNextWindowSize(
                [800., if self.show_full_error { 500. } else { 0. }].into(),
                Condition::Always as i32,
            );
        }

        ui.open_popup("#fatal-error");
        ui.modal_popup_config("#fatal-error")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .size(
                [800., if self.show_full_error { 500. } else { 0. }],
                Condition::Always,
            )
            .build(|| {
                ui.checkbox("Show full error", &mut self.show_full_error);
                ui.text_wrapped(if self.show_full_error {
                    format!("{:?}", error)
                } else {
                    error.to_string()
                });

                ui.separator();
                if ui.button("Exit") {
                    std::process::exit(1);
                }
            });
    }
}

impl<G: Game> ImguiRenderLoop for ErrorDisplay<G> {
    fn render(&mut self, ui: &mut Ui) {
        let hud_visible = self.hud_visible.load(Ordering::Relaxed);
        let io = ui.io();
        let mut flag = InputFlags::empty();

        // When the HUD is hidden, release any input it had captured. Keep
        // honoring ImGui's capture flags for the fatal-error popup, which is
        // intentionally independent of HUD visibility.
        if hud_visible || self.error.is_some() {
            if io.want_capture_mouse {
                flag |= InputFlags::Mouse;
            }
            if io.want_capture_keyboard {
                flag |= InputFlags::Keyboard;
            }
            if io.want_capture_mouse && io.want_capture_keyboard {
                // Only block pad input if both the mouse and keyboard are blocked
                // (for example if a modal dialog is up).
                flag |= InputFlags::GamePad;
            }
        }
        self.input_blocker.block_only(flag);

        if let Some(core) = &mut self.core {
            let mut core = core.lock().unwrap();
            if hud_visible {
                if let Some(overlay) = &mut self.overlay {
                    overlay.render(ui, &mut core);
                }
            }

            if self.error.is_none() {
                self.error = core.base_mut().take_error();
            }
        }

        self.render_error(ui);
    }

    /// Handle F1 before the game's window procedure. This works regardless of
    /// whether the ImGui HUD is focused or currently visible, and prevents the
    /// game itself from also receiving the toggle key.
    fn before_wnd_proc(
        &self,
        _hwnd: HWND,
        umsg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> BeforeWndProc {
        const VK_F1: usize = 0x70;
        const WM_KEYDOWN: u32 = 0x0100;
        const WM_KEYUP: u32 = 0x0101;
        const WM_SYSKEYDOWN: u32 = 0x0104;
        const WM_SYSKEYUP: u32 = 0x0105;

        if wparam.0 != VK_F1 {
            return BeforeWndProc::Continue;
        }

        match umsg {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                // Bit 30 is set on key-down auto-repeat messages. Only toggle
                // on the initial press, not while F1 is held down.
                if lparam.0 & (1_isize << 30) == 0 {
                    self.hud_visible.fetch_xor(true, Ordering::Relaxed);
                }
                BeforeWndProc::Break
            }
            WM_KEYUP | WM_SYSKEYUP => BeforeWndProc::Break,
            _ => BeforeWndProc::Continue,
        }
    }

    fn initialize<'a>(&'a mut self, ctx: &mut Context, _render_context: &'a mut dyn RenderContext) {
        ctx.set_clipboard_backend(crate::clipboard::WindowsClipboardBackend {});
    }

    fn before_render<'a>(
        &'a mut self,
        ctx: &mut Context,
        render_context: &'a mut dyn RenderContext,
    ) {
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.before_render(ctx, render_context);
        } else {
            // Set the font scale here to match the overlay's logic.
            ctx.io_mut().font_global_scale = 1.8;
        }
    }
}
