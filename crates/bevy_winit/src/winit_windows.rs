use bevy_a11y::AccessibilityRequested;
use bevy_ecs::entity::Entity;
use raw_window_handle::HasRawWindowHandle;

use bevy_ecs::entity::EntityHashMap;
use bevy_utils::{tracing::warn, HashMap};
use bevy_window::{
    CursorGrabMode, MonitorSelection, Window, WindowMode, WindowPosition, WindowResolution,
    WindowWrapper,
};

use crate::{
    accessibility::{
        prepare_accessibility_for_window, AccessKitAdapters, WinitActionRequestHandlers,
    },
    converters::{convert_enabled_buttons, convert_window_level, convert_window_theme},
    winit_monitors::WinitMonitors,
};
use bevy_hierarchy::Parent;
use winit::{
    dpi::{LogicalSize, PhysicalPosition},
    error::ExternalError,
    event_loop::ActiveEventLoop,
    monitor::{MonitorHandle, VideoModeHandle},
    window::{CursorGrabMode as WinitCursorGrabMode, Fullscreen, Window as WinitWindow, WindowId},
};

/// A resource mapping window entities to their `winit`-backend [`Window`](winit::window::Window)
/// states.
#[derive(Debug, Default)]
pub struct WinitWindows {
    /// Stores [`winit`] windows by window identifier.
    pub windows: HashMap<WindowId, WindowWrapper<WinitWindow>>,
    /// Maps entities to `winit` window identifiers.
    pub entity_to_winit: EntityHashMap<WindowId>,
    /// Maps `winit` window identifiers to entities.
    pub winit_to_entity: HashMap<WindowId, Entity>,
    // Many `winit` window functions (e.g. `set_window_icon`) can only be called on the main thread.
    // If they're called on other threads, the program might hang. This marker indicates that this
    // type is not thread-safe and will be `!Send` and `!Sync`.
    _not_send_sync: core::marker::PhantomData<*const ()>,
}

impl WinitWindows {
    /// Creates a `winit` window and associates it with our entity.
    #[allow(clippy::too_many_arguments)]
    pub fn create_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        entity: Entity,
        window: &Window,
        adapters: &mut AccessKitAdapters,
        handlers: &mut WinitActionRequestHandlers,
        accessibility_requested: &AccessibilityRequested,
        monitors: &WinitMonitors,
        parent_window_entity: Option<&Parent>,
    ) -> &WindowWrapper<WinitWindow> {
        let mut winit_window_attributes = WinitWindow::default_attributes();
        if let Some(parent_window_handle) = parent_window_entity
            .and_then(|parent| self.get_window(parent.get()))
            .and_then(|parent_window| parent_window.raw_window_handle().ok())
        {
            winit_window_attributes =
                unsafe { winit_window_attributes.with_parent_window(Some(parent_window_handle)) };
        }
        // Due to a UIA limitation, winit windows need to be invisible for the
        // AccessKit adapter is initialized.
        winit_window_attributes = winit_window_attributes.with_visible(false);

        let maybe_selected_monitor = &match window.mode {
            WindowMode::BorderlessFullscreen(monitor_selection)
            | WindowMode::Fullscreen(monitor_selection)
            | WindowMode::SizedFullscreen(monitor_selection) => select_monitor(
                monitors,
                event_loop.primary_monitor(),
                None,
                &monitor_selection,
            ),
            WindowMode::Windowed => None,
        };

        winit_window_attributes = match window.mode {
            WindowMode::BorderlessFullscreen(_) => winit_window_attributes
                .with_fullscreen(Some(Fullscreen::Borderless(maybe_selected_monitor.clone()))),
            WindowMode::Fullscreen(_) => {
                let select_monitor = &maybe_selected_monitor
                    .clone()
                    .expect("Unable to get monitor.");
                let videomode = get_best_videomode(select_monitor);
                winit_window_attributes.with_fullscreen(Some(Fullscreen::Exclusive(videomode)))
            }
            WindowMode::SizedFullscreen(_) => {
                let select_monitor = &maybe_selected_monitor
                    .clone()
                    .expect("Unable to get monitor.");
                let videomode = get_fitting_videomode(
                    select_monitor,
                    window.width() as u32,
                    window.height() as u32,
                );
                winit_window_attributes.with_fullscreen(Some(Fullscreen::Exclusive(videomode)))
            }
            WindowMode::Windowed => {
                if let Some(position) = winit_window_position(
                    &window.position,
                    &window.resolution,
                    monitors,
                    event_loop.primary_monitor(),
                    None,
                ) {
                    winit_window_attributes = winit_window_attributes.with_position(position);
                }
                let logical_size = LogicalSize::new(window.width(), window.height());
                if let Some(sf) = window.resolution.scale_factor_override() {
                    let inner_size = logical_size.to_physical::<f64>(sf.into());
                    winit_window_attributes.with_inner_size(inner_size)
                } else {
                    winit_window_attributes.with_inner_size(logical_size)
                }
            }
        };

        winit_window_attributes = winit_window_attributes
            .with_window_level(convert_window_level(window.window_level))
            .with_theme(window.window_theme.map(convert_window_theme))
            .with_resizable(window.resizable)
            .with_enabled_buttons(convert_enabled_buttons(window.enabled_buttons))
            .with_decorations(window.decorations)
            .with_transparent(window.transparent)
            .with_visible(window.visible);

        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            winit_window_attributes =
                winit_window_attributes.with_skip_taskbar(window.skip_taskbar);
        }

        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            winit_window_attributes = winit_window_attributes
                .with_movable_by_window_background(window.movable_by_window_background)
                .with_fullsize_content_view(window.fullsize_content_view)
                .with_has_shadow(window.has_shadow)
                .with_titlebar_hidden(!window.titlebar_shown)
                .with_titlebar_transparent(window.titlebar_transparent)
                .with_title_hidden(!window.titlebar_show_title)
                .with_titlebar_buttons_hidden(!window.titlebar_show_buttons);
        }

        #[cfg(target_os = "ios")]
        {
            use winit::platform::ios::WindowAttributesExtIOS;
            winit_window_attributes = winit_window_attributes
                .with_prefers_home_indicator_hidden(window.prefers_home_indicator_hidden);
        }

        let display_info = DisplayInfo {
            window_physical_resolution: (
                window.resolution.physical_width(),
                window.resolution.physical_height(),
            ),
            window_logical_resolution: (window.resolution.width(), window.resolution.height()),
            monitor_name: maybe_selected_monitor
                .as_ref()
                .and_then(MonitorHandle::name),
            scale_factor: maybe_selected_monitor
                .as_ref()
                .map(MonitorHandle::scale_factor),
            refresh_rate_millihertz: maybe_selected_monitor
                .as_ref()
                .and_then(MonitorHandle::refresh_rate_millihertz),
        };
        bevy_log::debug!("{display_info}");

        #[cfg(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "windows"
        ))]
        if let Some(name) = &window.name {
            #[cfg(all(
                feature = "wayland",
                any(
                    target_os = "linux",
                    target_os = "dragonfly",
                    target_os = "freebsd",
                    target_os = "netbsd",
                    target_os = "openbsd"
                )
            ))]
            {
                winit_window_attributes =
                    winit::platform::wayland::WindowAttributesExtWayland::with_name(
                        winit_window_attributes,
                        name.clone(),
                        "",
                    );
            }

            #[cfg(all(
                feature = "x11",
                any(
                    target_os = "linux",
                    target_os = "dragonfly",
                    target_os = "freebsd",
                    target_os = "netbsd",
                    target_os = "openbsd"
                )
            ))]
            {
                winit_window_attributes = winit::platform::x11::WindowAttributesExtX11::with_name(
                    winit_window_attributes,
                    name.clone(),
                    "",
                );
            }
            #[cfg(target_os = "windows")]
            {
                winit_window_attributes =
                    winit::platform::windows::WindowAttributesExtWindows::with_class_name(
                        winit_window_attributes,
                        name.clone(),
                    );
            }
        }

        let constraints = window.resize_constraints.check_constraints();
        let min_inner_size = LogicalSize {
            width: constraints.min_width,
            height: constraints.min_height,
        };
        let max_inner_size = LogicalSize {
            width: constraints.max_width,
            height: constraints.max_height,
        };

        let winit_window_attributes =
            if constraints.max_width.is_finite() && constraints.max_height.is_finite() {
                winit_window_attributes
                    .with_min_inner_size(min_inner_size)
                    .with_max_inner_size(max_inner_size)
            } else {
                winit_window_attributes.with_min_inner_size(min_inner_size)
            };

        #[allow(unused_mut)]
        let mut winit_window_attributes = winit_window_attributes.with_title(window.title.as_str());

        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            use winit::platform::web::WindowAttributesExtWebSys;

            if let Some(selector) = &window.canvas {
                let window = web_sys::window().unwrap();
                let document = window.document().unwrap();
                let canvas = document
                    .query_selector(selector)
                    .expect("Cannot query for canvas element.");
                if let Some(canvas) = canvas {
                    let canvas = canvas.dyn_into::<web_sys::HtmlCanvasElement>().ok();
                    winit_window_attributes = winit_window_attributes.with_canvas(canvas);
                } else {
                    panic!("Cannot find element: {}.", selector);
                }
            }

            winit_window_attributes =
                winit_window_attributes.with_prevent_default(window.prevent_default_event_handling);
            winit_window_attributes = winit_window_attributes.with_append(true);
        }

        let winit_window = event_loop.create_window(winit_window_attributes).unwrap();
        let name = window.title.clone();
        prepare_accessibility_for_window(
            &winit_window,
            entity,
            name,
            accessibility_requested.clone(),
            adapters,
            handlers,
        );

        // Do not set the grab mode on window creation if it's none. It can fail on mobile.
        if window.cursor_options.grab_mode != CursorGrabMode::None {
            let _ = attempt_grab(&winit_window, window.cursor_options.grab_mode);
        }

        winit_window.set_cursor_visible(window.cursor_options.visible);

        // Do not set the cursor hittest on window creation if it's false, as it will always fail on
        // some platforms and log an unfixable warning.
        if !window.cursor_options.hit_test {
            if let Err(err) = winit_window.set_cursor_hittest(window.cursor_options.hit_test) {
                warn!(
                    "Could not set cursor hit test for window {:?}: {:?}",
                    window.title, err
                );
            }
        }

        self.entity_to_winit.insert(entity, winit_window.id());
        self.winit_to_entity.insert(winit_window.id(), entity);

        self.windows
            .entry(winit_window.id())
            .insert(WindowWrapper::new(winit_window))
            .into_mut()
    }

    /// Get the winit window that is associated with our entity.
    pub fn get_window(&self, entity: Entity) -> Option<&WindowWrapper<WinitWindow>> {
        self.entity_to_winit
            .get(&entity)
            .and_then(|winit_id| self.windows.get(winit_id))
    }

    /// Get the entity associated with the winit window id.
    ///
    /// This is mostly just an intermediary step between us and winit.
    pub fn get_window_entity(&self, winit_id: WindowId) -> Option<Entity> {
        self.winit_to_entity.get(&winit_id).cloned()
    }

    /// Remove a window from winit.
    ///
    /// This should mostly just be called when the window is closing.
    pub fn remove_window(&mut self, entity: Entity) -> Option<WindowWrapper<WinitWindow>> {
        let winit_id = self.entity_to_winit.remove(&entity)?;
        self.winit_to_entity.remove(&winit_id);
        self.windows.remove(&winit_id)
    }
}

/// Gets the "best" video mode which fits the given dimensions.
///
/// The heuristic for "best" prioritizes width, height, and refresh rate in that order.
pub fn get_fitting_videomode(monitor: &MonitorHandle, width: u32, height: u32) -> VideoModeHandle {
    let mut modes = monitor.video_modes().collect::<Vec<_>>();

    fn abs_diff(a: u32, b: u32) -> u32 {
        if a > b {
            return a - b;
        }
        b - a
    }

    modes.sort_by(|a, b| {
        use core::cmp::Ordering::*;
        match abs_diff(a.size().width, width).cmp(&abs_diff(b.size().width, width)) {
            Equal => {
                match abs_diff(a.size().height, height).cmp(&abs_diff(b.size().height, height)) {
                    Equal => b
                        .refresh_rate_millihertz()
                        .cmp(&a.refresh_rate_millihertz()),
                    default => default,
                }
            }
            default => default,
        }
    });

    modes.first().unwrap().clone()
}

/// Gets the "best" video-mode handle from a monitor.
///
/// The heuristic for "best" prioritizes width, height, and refresh rate in that order.
pub fn get_best_videomode(monitor: &MonitorHandle) -> VideoModeHandle {
    let mut modes = monitor.video_modes().collect::<Vec<_>>();
    modes.sort_by(|a, b| {
        use core::cmp::Ordering::*;
        match b.size().width.cmp(&a.size().width) {
            Equal => match b.size().height.cmp(&a.size().height) {
                Equal => b
                    .refresh_rate_millihertz()
                    .cmp(&a.refresh_rate_millihertz()),
                default => default,
            },
            default => default,
        }
    });

    modes.first().unwrap().clone()
}

pub(crate) fn attempt_grab(
    winit_window: &WinitWindow,
    grab_mode: CursorGrabMode,
) -> Result<(), ExternalError> {
    let grab_result = match grab_mode {
        CursorGrabMode::None => winit_window.set_cursor_grab(WinitCursorGrabMode::None),
        CursorGrabMode::Confined => winit_window
            .set_cursor_grab(WinitCursorGrabMode::Confined)
            .or_else(|_e| winit_window.set_cursor_grab(WinitCursorGrabMode::Locked)),
        CursorGrabMode::Locked => winit_window
            .set_cursor_grab(WinitCursorGrabMode::Locked)
            .or_else(|_e| winit_window.set_cursor_grab(WinitCursorGrabMode::Confined)),
    };

    if let Err(err) = grab_result {
        let err_desc = match grab_mode {
            CursorGrabMode::Confined | CursorGrabMode::Locked => "grab",
            CursorGrabMode::None => "ungrab",
        };

        bevy_utils::tracing::error!("Unable to {} cursor: {}", err_desc, err);
        Err(err)
    } else {
        Ok(())
    }
}

/// Compute the physical window position for a given [`WindowPosition`].
// Ideally we could generify this across window backends, but we only really have winit atm
// so whatever.
pub fn winit_window_position(
    position: &WindowPosition,
    resolution: &WindowResolution,
    monitors: &WinitMonitors,
    primary_monitor: Option<MonitorHandle>,
    current_monitor: Option<MonitorHandle>,
) -> Option<PhysicalPosition<i32>> {
    match position {
        WindowPosition::Automatic => {
            // Window manager will handle position
            None
        }
        WindowPosition::Centered(monitor_selection) => {
            let maybe_monitor = select_monitor(
                monitors,
                primary_monitor,
                current_monitor,
                monitor_selection,
            );

            if let Some(monitor) = maybe_monitor {
                let screen_size = monitor.size();

                let scale_factor = match resolution.scale_factor_override() {
                    Some(scale_factor_override) => scale_factor_override as f64,
                    // We use the monitors scale factor here since `WindowResolution.scale_factor` is
                    // not yet populated when windows are created during plugin setup.
                    None => monitor.scale_factor(),
                };

                // Logical to physical window size
                let (width, height): (u32, u32) =
                    LogicalSize::new(resolution.width(), resolution.height())
                        .to_physical::<u32>(scale_factor)
                        .into();

                let position = PhysicalPosition {
                    x: screen_size.width.saturating_sub(width) as f64 / 2.
                        + monitor.position().x as f64,
                    y: screen_size.height.saturating_sub(height) as f64 / 2.
                        + monitor.position().y as f64,
                };

                Some(position.cast::<i32>())
            } else {
                warn!("Couldn't get monitor selected with: {monitor_selection:?}");
                None
            }
        }
        WindowPosition::At(position) => {
            Some(PhysicalPosition::new(position[0] as f64, position[1] as f64).cast::<i32>())
        }
    }
}

/// Selects a monitor based on the given [`MonitorSelection`].
pub fn select_monitor(
    monitors: &WinitMonitors,
    primary_monitor: Option<MonitorHandle>,
    current_monitor: Option<MonitorHandle>,
    monitor_selection: &MonitorSelection,
) -> Option<MonitorHandle> {
    use bevy_window::MonitorSelection::*;

    match monitor_selection {
        Current => {
            if current_monitor.is_none() {
                warn!("Can't select current monitor on window creation or cannot find current monitor!");
            }
            current_monitor
        }
        Primary => primary_monitor,
        Index(n) => monitors.nth(*n),
        Entity(entity) => monitors.find_entity(*entity),
    }
}

struct DisplayInfo {
    window_physical_resolution: (u32, u32),
    window_logical_resolution: (f32, f32),
    monitor_name: Option<String>,
    scale_factor: Option<f64>,
    refresh_rate_millihertz: Option<u32>,
}

impl core::fmt::Display for DisplayInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Display information:")?;
        write!(
            f,
            "  Window physical resolution: {}x{}",
            self.window_physical_resolution.0, self.window_physical_resolution.1
        )?;
        write!(
            f,
            "  Window logical resolution: {}x{}",
            self.window_logical_resolution.0, self.window_logical_resolution.1
        )?;
        write!(
            f,
            "  Monitor name: {}",
            self.monitor_name.as_deref().unwrap_or("")
        )?;
        write!(f, "  Scale factor: {}", self.scale_factor.unwrap_or(0.))?;
        let millihertz = self.refresh_rate_millihertz.unwrap_or(0);
        let hertz = millihertz / 1000;
        let extra_millihertz = millihertz % 1000;
        write!(f, "  Refresh rate (Hz): {}.{:03}", hertz, extra_millihertz)?;
        Ok(())
    }
}
