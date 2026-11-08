use crate::config::AppConfig;
use crate::ipc::{IpcCommand, IpcServer};
use crate::render::Renderer;
use crate::state::{AppState, HitTarget, SURFACE_HEIGHT, SURFACE_WIDTH};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::time::{Duration, Instant};

/// Linux input event button codes (`linux/input-event-codes.h`)
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

pub struct RunOptions {
    pub force_window: bool,
    pub start_hidden: bool,
    pub open_settings: bool,
    pub initial_prompt: Option<String>,
    pub auto_quit_ms: Option<u64>,
}

enum ShellSurface {
    Layer(LayerSurface),
    Xdg(Window),
}

impl ShellSurface {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Self::Layer(l) => l.wl_surface(),
            Self::Xdg(w) => w.wl_surface(),
        }
    }

    fn commit(&self) {
        match self {
            Self::Layer(l) => l.commit(),
            Self::Xdg(w) => w.commit(),
        }
    }
}

pub struct WaylandApp {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    shm: Shm,
    pool: SlotPool,

    shell: ShellSurface,
    configured: bool,
    frame_pending: bool,
    has_mapped: bool,
    handled_summon: u64,
    needs_redraw: bool,

    scale_factor: i32,
    width: u32,
    height: u32,

    keyboard: Option<wl_keyboard::WlKeyboard>,
    modifiers: Modifiers,
    themed_pointer: Option<ThemedPointer>,
    last_cursor: CursorIcon,

    current_kb_mode: KeyboardInteractivity,
    last_input_region: Option<(i32, i32, i32, i32, i32)>,

    loop_handle: smithay_client_toolkit::reexports::calloop::LoopHandle<'static, WaylandApp>,
    conn: Connection,
    app: AppState,
    renderer: Renderer,
    ipc: Option<IpcServer>,
    last_frame_instant: Instant,
    boot_instant: Instant,
    auto_quit_ms: Option<u64>,
}

pub fn run_wayland(
    config: AppConfig,
    ipc: Option<IpcServer>,
    opts: RunOptions,
) -> Result<(), String> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("Failed to connect to Wayland display: {e}"))?;
    let (globals, event_queue) =
        registry_queue_init(&conn).map_err(|e| format!("Wayland registry init failed: {e}"))?;
    let qh = event_queue.handle();

    let mut event_loop: EventLoop<WaylandApp> =
        EventLoop::try_new().map_err(|e| format!("calloop init failed: {e}"))?;
    let loop_handle = event_loop.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|e| format!("wl_compositor unavailable: {e}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm unavailable: {e}"))?;

    let surface = compositor.create_surface(&qh);

    let mut app_state = AppState::new(config);
    if opts.start_hidden {
        app_state.hide_overlay();
    }
    if opts.open_settings {
        app_state.open_settings();
    }
    if let Some(prompt) = opts.initial_prompt {
        if !prompt.trim().is_empty() {
            app_state.show_overlay(true);
            app_state.composer.set(prompt);
            app_state.submit_composer();
        }
    }

    let initial_kb_mode = if app_state.wants_keyboard_on_demand() {
        KeyboardInteractivity::OnDemand
    } else {
        KeyboardInteractivity::None
    };

    // Prefer `zwlr_layer_shell_v1` (native `niri` floating overlay); fall back cleanly to XDG window if requested or absent
    let shell = if !opts.force_window {
        if let Ok(layer_shell) = LayerShell::bind(&globals, &qh) {
            let layer = layer_shell.create_layer_surface(
                &qh,
                surface,
                Layer::Top,
                Some("thetalk"),
                None,
            );
            layer.set_anchor(Anchor::BOTTOM);
            layer.set_exclusive_zone(-1);
            layer.set_margin(0, 0, 14, 0);
            layer.set_size(SURFACE_WIDTH, SURFACE_HEIGHT);
            layer.set_keyboard_interactivity(initial_kb_mode);
            layer.commit();
            ShellSurface::Layer(layer)
        } else {
            create_xdg_fallback(&globals, &qh, surface)?
        }
    } else {
        create_xdg_fallback(&globals, &qh, surface)?
    };

    let pool_bytes = (SURFACE_WIDTH as usize) * (SURFACE_HEIGHT as usize) * 4 * 4;
    let pool =
        SlotPool::new(pool_bytes, &shm).map_err(|e| format!("wl_shm SlotPool failed: {e}"))?;

    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .map_err(|e| format!("WaylandSource insert failed: {e}"))?;

    let mut client = WaylandApp {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        shm,
        pool,
        shell,
        configured: false,
        frame_pending: false,
        has_mapped: false,
        handled_summon: 0,
        needs_redraw: true,
        scale_factor: 1,
        width: SURFACE_WIDTH,
        height: SURFACE_HEIGHT,
        keyboard: None,
        modifiers: Modifiers::default(),
        themed_pointer: None,
        last_cursor: CursorIcon::Default,
        current_kb_mode: initial_kb_mode,
        last_input_region: None,
        loop_handle,
        conn: conn.clone(),
        app: app_state,
        renderer: Renderer::new(1.0),
        ipc,
        last_frame_instant: Instant::now(),
        boot_instant: Instant::now(),
        auto_quit_ms: opts.auto_quit_ms,
    };

    loop {
        if let Some(limit_ms) = client.auto_quit_ms {
            if client.boot_instant.elapsed() >= Duration::from_millis(limit_ms) {
                break;
            }
        }

        // Poll Unix socket IPC commands (`toggle`, `show`, `hide`, `new`, `settings`, `send:...`, `quit`)
        let cmds = client
            .ipc
            .as_mut()
            .map(|srv| srv.poll_commands())
            .unwrap_or_default();
        if !cmds.is_empty() {
            for cmd in cmds {
                match cmd {
                    IpcCommand::Toggle => client.app.toggle_overlay(),
                    IpcCommand::Show => client.app.show_overlay(true),
                    IpcCommand::Hide => client.app.hide_overlay(),
                    IpcCommand::NewChat => {
                        client.app.reset_chat();
                        client.app.show_overlay(true);
                    }
                    IpcCommand::Settings => {
                        client.app.show_overlay(true);
                        client.app.open_settings();
                    }
                    IpcCommand::SendPrompt(p) => {
                        client.app.show_overlay(false);
                        client.app.composer.set(p);
                        client.app.submit_composer();
                    }
                    IpcCommand::Quit => {
                        client.app.should_quit = true;
                    }
                }
            }
            client.sync_layer_focus_and_region(true);
            client.needs_redraw = true;
        }

        client.apply_summon();

        // Step physics & streaming frontier
        let now_inst = Instant::now();
        let dt = now_inst
            .duration_since(client.last_frame_instant)
            .as_secs_f32();
        client.last_frame_instant = now_inst;

        let animating = client.app.tick(dt);
        if animating {
            client.needs_redraw = true;
        }

        // Blink cursor gently at ~30Hz only when focused on a text input
        let cursor_focused = client.app.visible && client.app.has_keyboard_focus;
        let timeout = if animating || client.needs_redraw {
            if client.frame_pending { Duration::from_millis(8) } else { Duration::ZERO }
        } else if cursor_focused {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(50)
        };

        if cursor_focused && !client.frame_pending {
            client.needs_redraw = true;
        }

        if client.configured && client.needs_redraw && !client.frame_pending {
            client.present_with_qh(&qh);
        }

        event_loop
            .dispatch(timeout, &mut client)
            .map_err(|e| format!("Event loop dispatch error: {e}"))?;

        let _ = client.conn.flush();

        if client.app.should_quit {
            break;
        }
    }

    Ok(())
}

fn create_xdg_fallback(
    globals: &smithay_client_toolkit::reexports::client::globals::GlobalList,
    qh: &QueueHandle<WaylandApp>,
    surface: wl_surface::WlSurface,
) -> Result<ShellSurface, String> {
    let xdg_shell =
        XdgShell::bind(globals, qh).map_err(|e| format!(" neither layer-shell nor xdg-shell available: {e}"))?;
    let window = xdg_shell.create_window(surface, WindowDecorations::None, qh);
    window.set_title("thetalk");
    window.set_app_id("thetalk");
    window.set_min_size(Some((SURFACE_WIDTH, SURFACE_HEIGHT)));
    window.set_max_size(Some((SURFACE_WIDTH, SURFACE_HEIGHT)));
    window.commit();
    Ok(ShellSurface::Xdg(window))
}

impl WaylandApp {
    /// niri focuses a newly mapped OnDemand surface, not a mode change on an
    /// already mapped one. Remap only for explicit summons, never for typing,
    /// hover, streaming, or keyboard leave. The null + initial commit handshake
    /// must complete before attaching another buffer.
    fn apply_summon(&mut self) {
        let generation = self.app.summon_generation;
        if generation == self.handled_summon {
            return;
        }
        self.handled_summon = generation;
        if !self.app.visible || self.app.has_keyboard_focus || !self.has_mapped {
            return;
        }
        if let ShellSurface::Layer(layer) = &self.shell {
            self.configured = false;
            self.frame_pending = false;
            let surface = layer.wl_surface();
            surface.attach(None, 0, 0);
            surface.commit();
            layer.set_layer(Layer::Top);
            layer.set_anchor(Anchor::BOTTOM);
            layer.set_exclusive_zone(-1);
            layer.set_margin(0, 0, 14, 0);
            layer.set_size(SURFACE_WIDTH, SURFACE_HEIGHT);
            layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            self.current_kb_mode = KeyboardInteractivity::OnDemand;
            self.last_input_region = None;
            layer.commit();
            self.needs_redraw = true;
        }
    }

    /// Synchronizes `zwlr_layer_surface_v1` keyboard interactivity (`OnDemand` vs `None`)
    /// and `wl_surface.set_input_region` (dynamic card hitbox mask) for zero `niri` hijacking.
    fn sync_layer_focus_and_region(&mut self, commit_if_changed: bool) -> bool {
        let mut changed = false;

        if let ShellSurface::Layer(layer) = &self.shell {
            let desired = if self.app.wants_keyboard_on_demand() {
                KeyboardInteractivity::OnDemand
            } else {
                KeyboardInteractivity::None
            };
            if desired != self.current_kb_mode {
                layer.set_keyboard_interactivity(desired);
                self.current_kb_mode = desired;
                changed = true;
            }

            let target_region = if self.app.visible && self.app.presence_spring.value > 0.04 {
                let c = self.app.card_rect.current();
                let lift_y = (1.0 - self.app.presence_spring.value.clamp(0.0, 1.0)) * 14.0;
                Some((
                    c.x.floor() as i32,
                    (c.y + lift_y).floor() as i32,
                    c.w.ceil().max(1.0) as i32,
                    c.h.ceil().max(1.0) as i32,
                    self.app.card_radius.value.round().max(0.0) as i32,
                ))
            } else {
                Some((0, 0, 0, 0, 0))
            };

            if target_region != self.last_input_region {
                if let Ok(region) = Region::new(&self.compositor) {
                    if let Some((rx, ry, rw, rh, radius)) = target_region {
                        if rw > 0 && rh > 0 {
                            let r = radius.min(rw / 2).min(rh / 2);
                            if rh > 2 * r { region.add(rx, ry + r, rw, rh - 2 * r); }
                            for y in 0..r {
                                let dy = r as f32 - y as f32 - 0.5;
                                let inset = (r as f32 - ((r * r) as f32 - dy * dy).sqrt()).ceil() as i32;
                                let width = rw - inset * 2;
                                if width > 0 {
                                    region.add(rx + inset, ry + y, width, 1);
                                    region.add(rx + inset, ry + rh - y - 1, width, 1);
                                }
                            }
                        }
                    }
                    layer
                        .wl_surface()
                        .set_input_region(Some(region.wl_region()));
                    self.last_input_region = target_region;
                    changed = true;
                }
            }
        }

        if changed && commit_if_changed {
            self.shell.commit();
        }
        changed
    }

    fn update_pointer_cursor(&mut self) {
        let desired = match self.app.hovered_hit() {
            Some(HitTarget::Input(_)) => CursorIcon::Text,
            Some(_) => CursorIcon::Pointer,
            None => CursorIcon::Default,
        };
        if desired != self.last_cursor {
            if let Some(tp) = self.themed_pointer.as_mut() {
                if tp.set_cursor(&self.conn, desired).is_ok() {
                    self.last_cursor = desired;
                }
            }
        }
    }

    fn render_and_attach(&mut self) {
        let scale = self.scale_factor.max(1);
        let phys_w = self.width.saturating_mul(scale as u32).max(1);
        let phys_h = self.height.saturating_mul(scale as u32).max(1);
        let stride = (phys_w as i32) * 4;

        self.renderer
            .ensure_size(self.width, self.height, scale as f32);
        self.renderer.render(&mut self.app);

        // Sync dynamic input region & `OnDemand`/`None` focus policy right before surface commit
        self.sync_layer_focus_and_region(false);

        let need_bytes = (phys_w as usize) * (phys_h as usize) * 4 * 2;
        if self.pool.len() < need_bytes {
            let _ = self.pool.resize(need_bytes);
        }

        let Ok((buffer, canvas)) = self.pool.create_buffer(
            phys_w as i32,
            phys_h as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) else {
            return;
        };

        if self.app.visible || self.app.presence_spring.value > 0.01 {
            self.renderer.copy_to_wayland_argb8888(canvas);
        } else {
            canvas.fill(0);
        }

        let wl_surf = self.shell.wl_surface();
        wl_surf.set_buffer_scale(scale);
        wl_surf.damage_buffer(0, 0, phys_w as i32, phys_h as i32);
        let _ = buffer.attach_to(wl_surf);
    }

    fn present_with_qh(&mut self, qh: &QueueHandle<Self>) {
        self.render_and_attach();
        self.has_mapped = true;
        let wl_surf = self.shell.wl_surface();
        wl_surf.frame(qh, wl_surf.clone());
        self.frame_pending = true;
        self.needs_redraw = false;
        self.shell.commit();
    }

}

impl CompositorHandler for WaylandApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if surface == self.shell.wl_surface() {
            self.scale_factor = new_factor.clamp(1, 4);
            self.needs_redraw = true;
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if surface == self.shell.wl_surface() {
            self.frame_pending = false;
            // Paint in the main loop after advancing the monotonic clock, not
            // inside dispatch with the previous tick's animation state.
        }
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for WaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for WaylandApp {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.app.should_quit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.width = if configure.new_size.0 == 0 {
            SURFACE_WIDTH
        } else {
            configure.new_size.0
        };
        self.height = if configure.new_size.1 == 0 {
            SURFACE_HEIGHT
        } else {
            configure.new_size.1
        };
        self.configured = true;
        self.needs_redraw = true;
    }
}

impl WindowHandler for WaylandApp {
    fn request_close(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _window: &Window) {
        self.app.should_quit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        self.width = configure
            .new_size
            .0
            .map(|v| v.get())
            .unwrap_or(SURFACE_WIDTH);
        self.height = configure
            .new_size
            .1
            .map(|v| v.get())
            .unwrap_or(SURFACE_HEIGHT);
        self.configured = true;
        self.needs_redraw = true;
    }
}

impl SeatHandler for WaylandApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(kbd) = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|client, _wl_kbd, event| {
                    let mods = client.modifiers;
                    client.app.handle_key(
                        event.keysym.raw(),
                        event.utf8.as_deref(),
                        mods.ctrl,
                        mods.alt,
                        mods.shift,
                    );
                    client.sync_layer_focus_and_region(true);
                    client.needs_redraw = true;
                }),
            ) {
                self.keyboard = Some(kbd);
            }
        }

        if capability == Capability::Pointer && self.themed_pointer.is_none() {
            let cursor_surface = self.compositor.create_surface(qh);
            if let Ok(tp) = self.seat_state.get_pointer_with_theme(
                qh,
                &seat,
                self.shm.wl_shm(),
                cursor_surface,
                ThemeSpec::default(),
            ) {
                self.themed_pointer = Some(tp);
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if capability == Capability::Pointer {
            if let Some(tp) = self.themed_pointer.take() {
                tp.pointer().release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for WaylandApp {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        if surface == self.shell.wl_surface() {
            self.app.has_keyboard_focus = true;
            self.app.last_cursor_activity = self.app.now_secs();
            self.sync_layer_focus_and_region(true);
            self.needs_redraw = true;
        }
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        if surface == self.shell.wl_surface() {
            self.app.has_keyboard_focus = false;
            // Ownership ended. Keep click eligibility; do not request focus again.
            self.sync_layer_focus_and_region(true);
            self.needs_redraw = true;
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        let mods = self.modifiers;
        self.app.handle_key(
            event.keysym.raw(),
            event.utf8.as_deref(),
            mods.ctrl,
            mods.alt,
            mods.shift,
        );
        self.sync_layer_focus_and_region(true);
        self.needs_redraw = true;
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _layout: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl PointerHandler for WaylandApp {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        use PointerEventKind::*;
        let mut dirty = false;

        for event in events {
            if &event.surface != self.shell.wl_surface() {
                continue;
            }
            match event.kind {
                Enter { .. } => {
                    self.app.pointer_pos =
                        Some((event.position.0 as f32, event.position.1 as f32));
                    self.last_cursor = CursorIcon::Wait; // force refresh on enter
                    self.update_pointer_cursor();
                    self.sync_layer_focus_and_region(true);
                    dirty = true;
                }
                Motion { .. } => {
                    let prev_hit = self.app.hovered_hit();
                    self.app.pointer_pos =
                        Some((event.position.0 as f32, event.position.1 as f32));
                    let next_hit = self.app.hovered_hit();
                    self.update_pointer_cursor();
                    self.sync_layer_focus_and_region(true);
                    if prev_hit != next_hit {
                        dirty = true;
                    }
                }
                Leave { .. } => {
                    self.app.pointer_pos = None;
                    self.sync_layer_focus_and_region(true);
                    dirty = true;
                }
                Press { button, .. } => {
                    self.app.pointer_pos =
                        Some((event.position.0 as f32, event.position.1 as f32));
                    self.app
                        .handle_click(button == BTN_LEFT, button == BTN_RIGHT);
                    self.sync_layer_focus_and_region(true);
                    dirty = true;
                }
                Release { .. } => {}
                Axis { horizontal, vertical, .. } => {
                    // Discrete wheel steps are not pixel deltas. Finger events
                    // have no detents: apply their logical deltas directly.
                    let wheel = vertical.discrete != 0 || horizontal.discrete != 0;
                    let dx = if horizontal.discrete != 0 { horizontal.discrete as f32 * 66.0 }
                        else { horizontal.absolute as f32 };
                    let dy = if vertical.discrete != 0 { vertical.discrete as f32 * 66.0 }
                        else { vertical.absolute as f32 };
                    if dx.abs() > 0.01 || dy.abs() > 0.01 {
                        self.app.scroll_input(dx, dy, !wheel, self.modifiers.shift);
                        dirty = true;
                    }
                }
            }
        }

        if dirty {
            self.needs_redraw = true;
        }
    }
}

impl ShmHandler for WaylandApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(WaylandApp);
delegate_output!(WaylandApp);
delegate_shm!(WaylandApp);
delegate_seat!(WaylandApp);
delegate_keyboard!(WaylandApp);
delegate_pointer!(WaylandApp);
delegate_layer!(WaylandApp);
delegate_xdg_shell!(WaylandApp);
delegate_xdg_window!(WaylandApp);
delegate_registry!(WaylandApp);

impl ProvidesRegistryState for WaylandApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
