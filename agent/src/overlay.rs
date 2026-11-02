// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Edge push-through effect.
//!
//! A click-through, topmost window covering the virtual desktop hosts one
//! DirectComposition visual per active "site" on a screen edge. Each site is
//! drawn with Direct2D and animated by springs (see `spring.rs`) on this
//! module's own thread, paced to the display with `DwmFlush`. The window is
//! hidden whenever nothing is on screen.
//!
//! Effects, as seen on the device the cursor is moving into:
//! - approach: a soft bump grows out of the edge while the user pushes, with
//!   the pointer dot riding at its tip;
//! - release: the bump springs back if the user stops before passing;
//! - pass: the bump snaps away as the cursor leaves through it;
//! - emerge: where the cursor comes in, the membrane flicks back with a slight
//!   wobble while the dot shrinks into the pointer and a ring fades out.

use crate::effect::{EffectParams, EffectStore};
use crate::spring::Spring;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1Shadow, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DEBUG_LEVEL_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_VECTOR4, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION,
    D2D1_SHADOW_PROP_COLOR, D2D1CreateFactory, ID2D1Brush, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1PathGeometry1, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDesktopDevice, IDCompositionSurface,
    IDCompositionTarget, IDCompositionVisual, IDCompositionVisual2,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetSystemMetrics, HTTRANSPARENT,
    HWND_TOPMOST, LWA_ALPHA, MSG, MsgWaitForMultipleObjects, PM_NOREMOVE, PM_REMOVE, PeekMessageW,
    PostThreadMessageW, QS_ALLINPUT, RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage, WM_APP, WM_NCHITTEST,
    WM_QUIT, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Interface, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

/// Where the pointer is placed inside the screen after it comes through an
/// edge, in physical pixels. The emerge dot shrinks onto this point.
pub const ENTRY_INSET: i32 = 2;

const WAKE: u32 = WM_APP + 7;
const RIPPLE_SECONDS: f32 = 0.6;
/// Final ripple radius as a multiple of the dot size.
const RIPPLE_REACH: f32 = 2.2;
/// The emerge settles a little slower than the push so the eye can follow it.
const EMERGE_RESPONSE_SCALE: f32 = 1.35;
const BUMP_SEGMENTS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn opposite(self) -> Self {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// A point on an edge line in virtual-desktop pixels. For the right and
/// bottom edges the line is the monitor's exclusive right/bottom coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgePoint {
    pub edge: Edge,
    pub x: i32,
    pub y: i32,
}

impl EdgePoint {
    /// The pixel just inside the screen, used to pick the monitor.
    fn inside(self) -> POINT {
        match self.edge {
            Edge::Right => POINT {
                x: self.x - 1,
                y: self.y,
            },
            Edge::Bottom => POINT {
                x: self.x,
                y: self.y - 1,
            },
            Edge::Left | Edge::Top => POINT {
                x: self.x,
                y: self.y,
            },
        }
    }
}

pub enum Command {
    /// The user pushes against an edge; `progress` reaches 1 at the crossing.
    Approach {
        at: EdgePoint,
        progress: f32,
    },
    /// The user stopped pushing without crossing.
    Release,
    /// The cursor left through the approach bump.
    Pass,
    /// The cursor came in through an edge.
    Emerge {
        at: EdgePoint,
    },
    /// Plays push → pass → emerge on the primary monitor. With `hold`, the
    /// approach freezes at that progress until the next command.
    Preview {
        hold: Option<f32>,
    },
    Quit,
}

/// Cheap, cloneable sender for overlay commands.
#[derive(Clone)]
pub struct OverlayHandle {
    sender: mpsc::Sender<Command>,
    thread_id: u32,
    sleeping: Arc<AtomicBool>,
}

impl OverlayHandle {
    pub fn send(&self, command: Command) {
        // Only wake the thread when it is blocked; while animating it drains
        // the channel every frame, so high-rate mouse input costs no messages.
        if self.sender.send(command).is_ok() && self.sleeping.swap(false, Ordering::SeqCst) {
            // SAFETY: posting to a thread that owns a message queue.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WAKE, WPARAM(0), LPARAM(0));
            }
        }
    }
}

pub struct Overlay {
    handle: OverlayHandle,
    join: Option<JoinHandle<()>>,
}

impl Overlay {
    pub fn spawn(store: EffectStore) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let sleeping = Arc::new(AtomicBool::new(false));
        let thread_sleeping = sleeping.clone();
        let join = std::thread::Builder::new()
            .name("rinflow-overlay".to_owned())
            .spawn(move || {
                // SAFETY: creates this thread's message queue before its id is
                // published, so PostThreadMessageW cannot fail with no queue.
                let thread_id = unsafe {
                    let mut message = MSG::default();
                    let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                    GetCurrentThreadId()
                };
                let _ = ready_tx.send(thread_id);
                run(receiver, store, thread_sleeping);
            })?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| std::io::Error::other("overlay thread exited during start-up"))?;
        Ok(Self {
            handle: OverlayHandle {
                sender,
                thread_id,
                sleeping,
            },
            join: Some(join),
        })
    }

    pub fn handle(&self) -> OverlayHandle {
        self.handle.clone()
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.handle.send(Command::Quit);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn run(receiver: mpsc::Receiver<Command>, store: EffectStore, sleeping: Arc<AtomicBool>) {
    let mut renderer = match Renderer::new(store) {
        Ok(renderer) => renderer,
        Err(error) => {
            log!("overlay unavailable: {error}");
            // Keep draining so senders never block on a dead thread.
            while let Ok(command) = receiver.recv() {
                if matches!(command, Command::Quit) {
                    return;
                }
            }
            return;
        }
    };
    let mut last_frame = Instant::now();

    loop {
        // SAFETY: standard message pump for this thread's window and queue.
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                if message.message == WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        while let Ok(command) = receiver.try_recv() {
            if matches!(command, Command::Quit) {
                return;
            }
            renderer.apply(command);
        }

        if renderer.needs_frame() {
            // SAFETY: blocks until the compositor's next frame.
            if unsafe { DwmFlush() }.is_err() {
                std::thread::sleep(Duration::from_millis(8));
            }
            let now = Instant::now();
            let elapsed = now - last_frame;
            // DwmFlush returns at once while the desktop is not composing;
            // cap the loop instead of spinning.
            if elapsed < Duration::from_millis(4) {
                std::thread::sleep(Duration::from_millis(4) - elapsed);
            }
            let now = Instant::now();
            let dt = (now - last_frame).as_secs_f32().min(1.0 / 20.0);
            last_frame = now;
            renderer.step(dt);
            renderer.render();
        } else {
            renderer.hide_if_idle();
            sleeping.store(true, Ordering::SeqCst);
            // A command sent between the drain above and this point would
            // otherwise wait for the next unrelated message.
            match receiver.try_recv() {
                Ok(Command::Quit) => return,
                Ok(command) => {
                    sleeping.store(false, Ordering::SeqCst);
                    renderer.apply(command);
                }
                Err(_) => {
                    // SAFETY: waits for any message posted to this thread.
                    unsafe {
                        MsgWaitForMultipleObjects(None, false, INFINITE, QS_ALLINPUT);
                    }
                    sleeping.store(false, Ordering::SeqCst);
                }
            }
            last_frame = Instant::now();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Approach,
    Releasing,
    Passing,
    Emerging,
}

struct Site {
    phase: Phase,
    at: EdgePoint,
    /// Monitor DPI scale; all springs are in points and scaled when drawn.
    scale: f32,
    depth: Spring,
    dot: Spring,
    dot_alpha: Spring,
    /// Inward position of the dot centre while emerging.
    dot_inset: Spring,
    ripple: Option<f32>,
    /// Multiplies the spring response; the pass snap is quicker than the rest.
    response_scale: f32,
    surface: Option<SiteSurface>,
}

struct SiteSurface {
    visual: IDCompositionVisual2,
    surface: IDCompositionSurface,
    width: u32,
    height: u32,
}

impl Site {
    fn new(phase: Phase, at: EdgePoint) -> Self {
        Self {
            phase,
            at,
            scale: monitor_scale(at.inside()),
            depth: Spring::at(0.0),
            dot: Spring::at(0.0),
            dot_alpha: Spring::at(0.0),
            dot_inset: Spring::at(0.0),
            ripple: None,
            response_scale: 1.0,
            surface: None,
        }
    }

    fn springs_mut(&mut self) -> [&mut Spring; 4] {
        [
            &mut self.depth,
            &mut self.dot,
            &mut self.dot_alpha,
            &mut self.dot_inset,
        ]
    }

    fn is_settled(&self) -> bool {
        self.depth.is_settled(0.05)
            && self.dot.is_settled(0.002)
            && self.dot_alpha.is_settled(0.002)
            && self.dot_inset.is_settled(0.05)
            && self.ripple.is_none_or(|t| t >= RIPPLE_SECONDS)
    }

    fn is_finished(&self) -> bool {
        self.phase != Phase::Approach && self.is_settled() && self.depth.target <= 0.0
    }

    /// Centre of the pointer dot, in points from the edge.
    fn dot_position(&self) -> f32 {
        match self.phase {
            Phase::Emerging => self.dot_inset.value,
            _ => self.depth.value.max(0.0) * 0.5,
        }
    }
}

struct Preview {
    elapsed: f32,
    hold: Option<f32>,
    right: EdgePoint,
    left: EdgePoint,
    passed: bool,
}

struct Graphics {
    factory: ID2D1Factory1,
    device: IDCompositionDesktopDevice,
    _target: IDCompositionTarget,
    root: IDCompositionVisual2,
}

struct Renderer {
    store: EffectStore,
    params: EffectParams,
    light: bool,
    window: HWND,
    shown: bool,
    origin: POINT,
    graphics: Option<Graphics>,
    sites: Vec<Site>,
    preview: Option<Preview>,
    /// A command changed something that has to be drawn.
    dirty: bool,
}

impl Renderer {
    fn new(store: EffectStore) -> Result<Self> {
        let params = store.get();
        Ok(Self {
            store,
            params,
            light: crate::win::apps_use_light_theme(),
            window: create_window()?,
            shown: false,
            origin: POINT::default(),
            graphics: None,
            sites: Vec::new(),
            preview: None,
            dirty: false,
        })
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Approach { at, progress } => {
                self.preview = None;
                self.approach(at, progress);
            }
            Command::Release => {
                self.preview = None;
                self.release();
            }
            Command::Pass => self.pass(),
            Command::Emerge { at } => self.emerge(at),
            Command::Preview { hold } => {
                self.release();
                self.preview = Some(Preview::on_primary_monitor(hold));
            }
            Command::Quit => {}
        }
        self.dirty = true;
    }

    fn refresh_style(&mut self) {
        if self.sites.is_empty() {
            self.params = self.store.get();
            self.light = match self.params.theme.as_str() {
                "light" => true,
                "dark" => false,
                _ => crate::win::apps_use_light_theme(),
            };
        } else {
            // Live tuning: pick up slider changes while an effect is on screen.
            self.params = self.store.get();
        }
    }

    fn approach(&mut self, at: EdgePoint, progress: f32) {
        self.refresh_style();
        let progress = progress.clamp(0.0, 1.0);
        let index = match self
            .sites
            .iter()
            .position(|site| site.phase == Phase::Approach)
        {
            Some(index) if self.sites[index].at.edge == at.edge => index,
            Some(index) => {
                self.sites[index].begin_release();
                self.sites.push(Site::new(Phase::Approach, at));
                self.sites.len() - 1
            }
            None => {
                self.sites.push(Site::new(Phase::Approach, at));
                self.sites.len() - 1
            }
        };
        let site = &mut self.sites[index];
        site.at = at;
        // Rubber band: quick at first, then harder to push as it nears the
        // crossing point, which is where the "resistance" is felt.
        let stretch = 1.0 - (1.0 - progress).powi(2);
        site.depth.target = self.params.bump_depth * stretch;
        let reveal = smoothstep(0.25, 0.95, progress);
        site.dot.target = reveal;
        site.dot_alpha.target = reveal;
    }

    fn release(&mut self) {
        for site in &mut self.sites {
            if site.phase == Phase::Approach {
                site.begin_release();
            }
        }
    }

    fn pass(&mut self) {
        let kick = self.params.bump_depth * 9.0;
        for site in &mut self.sites {
            if site.phase == Phase::Approach {
                site.phase = Phase::Passing;
                site.response_scale = 0.7;
                // A short outward flick before the membrane closes reads as
                // the pointer breaking through.
                site.depth.velocity += kick;
                site.depth.target = 0.0;
                site.dot.target = 1.6;
                site.dot_alpha.target = 0.0;
            }
        }
    }

    fn emerge(&mut self, at: EdgePoint) {
        self.refresh_style();
        let mut site = Site::new(Phase::Emerging, at);
        site.response_scale = EMERGE_RESPONSE_SCALE;
        let depth = self.params.bump_depth * 0.9;
        site.depth = Spring::at(depth);
        site.depth.target = 0.0;
        // The dot stays opaque and shrinks onto the pointer's hotspot, which
        // reads as the pointer itself arriving rather than a flash.
        site.dot = Spring::at(1.0);
        site.dot.target = 0.0;
        site.dot_alpha = Spring::at(1.0);
        site.dot_inset = Spring::at(depth * 0.5);
        site.dot_inset.target = ENTRY_INSET as f32 / site.scale;
        site.ripple = self.params.ripple.then_some(0.0);
        self.sites.push(site);
    }

    fn needs_frame(&self) -> bool {
        self.dirty || self.preview.is_some() || self.sites.iter().any(|site| !site.is_settled())
    }

    fn step(&mut self, dt: f32) {
        self.step_preview(dt);
        let response = self.params.spring_response;
        let damping = self.params.spring_damping;
        for site in &mut self.sites {
            let response = response * site.response_scale;
            for spring in site.springs_mut() {
                spring.step(dt, response, damping);
            }
            if let Some(t) = &mut site.ripple {
                *t += dt;
            }
        }
        if let Some(graphics) = &self.graphics {
            for site in self.sites.iter().filter(|site| site.is_finished()) {
                if let Some(surface) = &site.surface {
                    // SAFETY: the visual was added to this root.
                    unsafe {
                        let _ = graphics.root.RemoveVisual(&surface.visual);
                    }
                }
            }
        }
        self.sites.retain(|site| !site.is_finished());
    }

    fn step_preview(&mut self, dt: f32) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        preview.elapsed += dt;
        let (right, left, elapsed) = (preview.right, preview.left, preview.elapsed);
        if let Some(hold) = preview.hold {
            self.approach(right, hold);
            return;
        }
        const PUSH: f32 = 0.9;
        if elapsed < PUSH {
            self.approach(right, ease_in_out(elapsed / PUSH) * 0.97);
        } else if !preview.passed {
            preview.passed = true;
            self.pass();
            self.emerge(left);
        } else if elapsed > PUSH + 1.6 {
            self.preview = None;
        }
    }

    fn render(&mut self) {
        self.dirty = false;
        if self.sites.is_empty() {
            self.commit();
            return;
        }
        self.show();
        if self.graphics.is_none() {
            match Graphics::new(self.window) {
                Ok(graphics) => self.graphics = Some(graphics),
                Err(error) => {
                    log!("overlay graphics failed: {error}");
                    self.sites.clear();
                    return;
                }
            }
        }
        if let Err(error) = self.draw_sites() {
            // Device lost (driver update, adapter change): rebuild next frame.
            log!("overlay draw failed, recreating: {error}");
            self.graphics = None;
            for site in &mut self.sites {
                site.surface = None;
            }
        }
    }

    fn draw_sites(&mut self) -> Result<()> {
        let graphics = self.graphics.as_ref().expect("graphics created above");
        for site in &mut self.sites {
            draw_site(graphics, site, &self.params, self.light, self.origin)?;
        }
        // SAFETY: commits the batch built above on this thread's device.
        unsafe { graphics.device.Commit() }
    }

    fn commit(&self) {
        if let Some(graphics) = &self.graphics {
            // SAFETY: commits pending visual removals.
            unsafe {
                let _ = graphics.device.Commit();
            }
        }
    }

    fn show(&mut self) {
        if self.shown {
            return;
        }
        // SAFETY: repositions this thread's own window over the virtual desktop.
        unsafe {
            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            self.origin = POINT { x, y };
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        self.shown = true;
    }

    fn hide_if_idle(&mut self) {
        if self.shown && self.sites.is_empty() {
            // SAFETY: hides this thread's own window.
            unsafe {
                let _ = ShowWindow(self.window, SW_HIDE);
            }
            self.shown = false;
        }
    }
}

impl Site {
    fn begin_release(&mut self) {
        self.phase = Phase::Releasing;
        self.depth.target = 0.0;
        self.dot.target = 0.0;
        self.dot_alpha.target = 0.0;
    }
}

impl Preview {
    fn on_primary_monitor(hold: Option<f32>) -> Self {
        let rect = monitor_rect(POINT { x: 0, y: 0 }, true);
        let y = (rect.top + rect.bottom) / 2;
        Self {
            elapsed: 0.0,
            hold: hold.map(|value| value.clamp(0.0, 1.0)),
            right: EdgePoint {
                edge: Edge::Right,
                x: rect.right,
                y,
            },
            left: EdgePoint {
                edge: Edge::Left,
                x: rect.left,
                y,
            },
            passed: false,
        }
    }
}

impl Graphics {
    fn new(window: HWND) -> Result<Self> {
        // SAFETY: standard D3D11 → D2D → DirectComposition device chain.
        unsafe {
            let d3d = create_d3d_device(D3D_DRIVER_TYPE_HARDWARE)
                .or_else(|_| create_d3d_device(D3D_DRIVER_TYPE_WARP))?;
            let dxgi: IDXGIDevice = d3d.cast()?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                Some(&D2D1_FACTORY_OPTIONS {
                    debugLevel: D2D1_DEBUG_LEVEL_NONE,
                }),
            )?;
            let d2d = factory.CreateDevice(&dxgi)?;
            let device: IDCompositionDesktopDevice = DCompositionCreateDevice2(&d2d)?;
            let target = device.CreateTargetForHwnd(window, true)?;
            let root = device.CreateVisual()?;
            target.SetRoot(&root)?;
            device.Commit()?;
            Ok(Self {
                factory,
                device,
                _target: target,
                root,
            })
        }
    }
}

unsafe fn create_d3d_device(driver: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    // SAFETY: out-parameter is a valid Option slot.
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
}

/// Inward extent and half length along the edge of a site's surface, in pixels.
fn surface_extent(params: &EffectParams, scale: f32) -> (f32, f32) {
    let shadow = 18.0 * scale;
    let ripple = params.dot_size * RIPPLE_REACH + 2.0;
    let inward = (params.bump_depth * 1.6).max(ripple + 4.0) * scale + shadow;
    let along = params.bump_width.max(ripple) * scale + shadow;
    (inward.ceil(), along.ceil())
}

fn draw_site(
    graphics: &Graphics,
    site: &mut Site,
    params: &EffectParams,
    light: bool,
    origin: POINT,
) -> Result<()> {
    let (inward, along) = surface_extent(params, site.scale);
    let (width, height) = if site.at.edge.is_vertical() {
        (inward as u32, (along * 2.0) as u32)
    } else {
        ((along * 2.0) as u32, inward as u32)
    };

    // SAFETY: all calls go to devices owned by this thread.
    unsafe {
        if site
            .surface
            .as_ref()
            .is_none_or(|s| s.width != width || s.height != height)
        {
            if let Some(old) = site.surface.take() {
                let _ = graphics.root.RemoveVisual(&old.visual);
            }
            let visual = graphics.device.CreateVisual()?;
            let surface = graphics.device.CreateSurface(
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            )?;
            visual.SetContent(&surface)?;
            graphics
                .root
                .AddVisual(&visual, true, None::<&IDCompositionVisual>)?;
            site.surface = Some(SiteSurface {
                visual,
                surface,
                width,
                height,
            });
        }
        let target = site.surface.as_ref().expect("surface created above");

        let (left, top) = match site.at.edge {
            Edge::Left => (site.at.x, site.at.y - along as i32),
            Edge::Right => (site.at.x - inward as i32, site.at.y - along as i32),
            Edge::Top => (site.at.x - along as i32, site.at.y),
            Edge::Bottom => (site.at.x - along as i32, site.at.y - inward as i32),
        };
        target.visual.SetOffsetX2((left - origin.x) as f32)?;
        target.visual.SetOffsetY2((top - origin.y) as f32)?;

        let mut offset = POINT::default();
        let context: ID2D1DeviceContext = target.surface.BeginDraw(None, &mut offset)?;
        let drawn = paint(
            &graphics.factory,
            &context,
            site,
            params,
            light,
            offset,
            (width, height),
            (inward, along),
        );
        target.surface.EndDraw()?;
        drawn
    }
}

struct Palette {
    fill: D2D1_COLOR_F,
    stroke: D2D1_COLOR_F,
    ink: D2D1_COLOR_F,
    /// Contrasting rim around the dot and ripple, like the outline of the
    /// system pointer, so they stay visible over both light and dark content.
    halo: D2D1_COLOR_F,
    shadow: D2D1_COLOR_F,
}

impl Palette {
    fn new(light: bool, opacity: f32) -> Self {
        if light {
            Self {
                fill: rgba(0.985, 0.985, 0.99, 0.88 * opacity),
                stroke: rgba(0.0, 0.0, 0.0, 0.10 * opacity),
                ink: rgba(0.11, 0.11, 0.13, 0.85 * opacity),
                halo: rgba(1.0, 1.0, 1.0, 0.85 * opacity),
                shadow: rgba(0.0, 0.0, 0.0, 0.22 * opacity),
            }
        } else {
            Self {
                fill: rgba(0.13, 0.13, 0.15, 0.82 * opacity),
                stroke: rgba(1.0, 1.0, 1.0, 0.18 * opacity),
                ink: rgba(1.0, 1.0, 1.0, 0.95 * opacity),
                halo: rgba(0.0, 0.0, 0.0, 0.45 * opacity),
                shadow: rgba(0.0, 0.0, 0.0, 0.45 * opacity),
            }
        }
    }
}

fn faded(color: D2D1_COLOR_F, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        a: color.a * alpha,
        ..color
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn paint(
    factory: &ID2D1Factory1,
    context: &ID2D1DeviceContext,
    site: &Site,
    params: &EffectParams,
    light: bool,
    offset: POINT,
    (width, height): (u32, u32),
    (inward, along): (f32, f32),
) -> Result<()> {
    let scale = site.scale;
    let palette = Palette::new(light, params.opacity);
    let depth = site.depth.value.max(0.0) * scale;
    let half = params.bump_width * scale;

    // SAFETY: drawing on the surface context handed out by BeginDraw.
    unsafe {
        context.SetDpi(96.0, 96.0);
        let identity = Matrix3x2::identity();
        let geometry = if depth > 0.25 {
            Some(bump_geometry(factory, depth, half)?)
        } else {
            None
        };

        // The shadow input is recorded before the clip is pushed: clips are
        // part of the target's state and must not leak into the command list.
        let shadow = match &geometry {
            Some(geometry) => {
                let surface_target = context.GetTarget()?;
                let list = context.CreateCommandList()?;
                context.SetTarget(&list);
                context.SetTransform(&identity);
                let solid = context.CreateSolidColorBrush(&rgba(0.0, 0.0, 0.0, 1.0), None)?;
                context.FillGeometry(geometry, &solid, None::<&ID2D1Brush>);
                list.Close()?;
                context.SetTarget(&surface_target);

                let effect = context.CreateEffect(&CLSID_D2D1Shadow)?;
                effect.SetValue(
                    D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
                    D2D1_PROPERTY_TYPE_FLOAT,
                    &(6.0 * scale).to_le_bytes(),
                )?;
                effect.SetValue(
                    D2D1_SHADOW_PROP_COLOR.0 as u32,
                    D2D1_PROPERTY_TYPE_VECTOR4,
                    &vector4_bytes(palette.shadow),
                )?;
                effect.SetInput(0, &list, true);
                Some(effect.GetOutput()?)
            }
            None => None,
        };

        // DirectComposition surfaces live in a shared atlas at `offset`;
        // clearing without a clip would wipe neighbouring surfaces.
        context.SetTransform(&identity);
        let clip = D2D_RECT_F {
            left: offset.x as f32,
            top: offset.y as f32,
            right: (offset.x + width as i32) as f32,
            bottom: (offset.y + height as i32) as f32,
        };
        context.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
        context.Clear(Some(&rgba(0.0, 0.0, 0.0, 0.0)));
        context.SetTransform(&edge_transform(site.at.edge, offset, inward, along));

        if let Some(shadow) = &shadow {
            context.DrawImage(
                shadow,
                None,
                None,
                D2D1_INTERPOLATION_MODE_LINEAR,
                D2D1_COMPOSITE_MODE_SOURCE_OVER,
            );
        }
        if let Some(geometry) = &geometry {
            let fill = context.CreateSolidColorBrush(&palette.fill, None)?;
            context.FillGeometry(geometry, &fill, None::<&ID2D1Brush>);
            let stroke = context.CreateSolidColorBrush(&palette.stroke, None)?;
            context.DrawGeometry(geometry, &stroke, scale, None::<&ID2D1StrokeStyle>);
        }

        if let Some(t) = site.ripple.filter(|t| *t < RIPPLE_SECONDS) {
            let k = t / RIPPLE_SECONDS;
            let eased = 1.0 - (1.0 - k).powi(3);
            let start = params.dot_size * 0.5;
            let end = params.dot_size * RIPPLE_REACH;
            let radius = (start + (end - start) * eased) * scale;
            let alpha = 0.6 * (1.0 - k) * (1.0 - k);
            let center = Vector2 {
                X: ENTRY_INSET as f32,
                Y: 0.0,
            };
            let halo = context.CreateSolidColorBrush(&faded(palette.halo, alpha), None)?;
            context.DrawEllipse(
                &ellipse(center, radius),
                &halo,
                3.5 * scale,
                None::<&ID2D1StrokeStyle>,
            );
            let ink = context.CreateSolidColorBrush(&faded(palette.ink, alpha), None)?;
            context.DrawEllipse(
                &ellipse(center, radius),
                &ink,
                1.5 * scale,
                None::<&ID2D1StrokeStyle>,
            );
        }

        let alpha = site.dot_alpha.value.clamp(0.0, 1.0);
        let radius = params.dot_size * 0.5 * scale * site.dot.value.max(0.0);
        if radius > 0.2 && alpha > 0.004 {
            let center = Vector2 {
                X: site.dot_position() * scale,
                Y: 0.0,
            };
            let halo = context.CreateSolidColorBrush(&faded(palette.halo, alpha), None)?;
            context.FillEllipse(&ellipse(center, radius + 1.5 * scale), &halo);
            let ink = context.CreateSolidColorBrush(&faded(palette.ink, alpha), None)?;
            context.FillEllipse(&ellipse(center, radius), &ink);
        }

        context.SetTransform(&identity);
        context.PopAxisAlignedClip();
    }
    Ok(())
}

/// The bump outline in edge-local coordinates: x points into the screen, y
/// runs along the edge. A (1 − t²)² profile meets the edge with zero slope, so
/// the shoulders blend into it like a stretched membrane.
unsafe fn bump_geometry(
    factory: &ID2D1Factory1,
    depth: f32,
    half: f32,
) -> Result<ID2D1PathGeometry1> {
    // SAFETY: builds a geometry with the factory that created the device.
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        // Start a few pixels outside the screen so no seam shows at the edge.
        sink.BeginFigure(Vector2 { X: -4.0, Y: -half }, D2D1_FIGURE_BEGIN_FILLED);
        let mut points = Vec::with_capacity(BUMP_SEGMENTS + 2);
        for index in 0..=BUMP_SEGMENTS {
            let t = -1.0 + 2.0 * index as f32 / BUMP_SEGMENTS as f32;
            let k = 1.0 - t * t;
            points.push(Vector2 {
                X: depth * k * k,
                Y: t * half,
            });
        }
        points.push(Vector2 { X: -4.0, Y: half });
        sink.AddLines(&points);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geometry)
    }
}

/// Maps edge-local coordinates (x inward, y along the edge, origin at the
/// site's point on the edge) to pixels in the site's surface.
fn edge_transform(edge: Edge, offset: POINT, inward: f32, along: f32) -> Matrix3x2 {
    let (ox, oy) = (offset.x as f32, offset.y as f32);
    let (m11, m12, m21, m22, dx, dy) = match edge {
        Edge::Left => (1.0, 0.0, 0.0, 1.0, 0.0, along),
        Edge::Right => (-1.0, 0.0, 0.0, 1.0, inward, along),
        Edge::Top => (0.0, 1.0, 1.0, 0.0, along, 0.0),
        Edge::Bottom => (0.0, -1.0, 1.0, 0.0, along, inward),
    };
    Matrix3x2 {
        M11: m11,
        M12: m12,
        M21: m21,
        M22: m22,
        M31: ox + dx,
        M32: oy + dy,
    }
}

fn create_window() -> Result<HWND> {
    // SAFETY: registers a class and creates a hidden popup on this thread.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("RinflowEdgeOverlay");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&class);
        // NOREDIRECTIONBITMAP: content comes only from DirectComposition.
        // LAYERED + TRANSPARENT: clicks go to whatever is underneath.
        let window = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP
                | WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE,
            class_name,
            w!("Rinflow edge effect"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        SetLayeredWindowAttributes(window, COLORREF(0), 255, LWA_ALPHA)?;
        Ok(window)
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    // SAFETY: default handling for everything else.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn monitor_rect(point: POINT, primary: bool) -> RECT {
    // SAFETY: plain monitor queries.
    unsafe {
        let monitor = MonitorFromPoint(
            point,
            if primary {
                MONITOR_DEFAULTTOPRIMARY
            } else {
                MONITOR_DEFAULTTONEAREST
            },
        );
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        info.rcMonitor
    }
}

fn monitor_scale(point: POINT) -> f32 {
    // SAFETY: plain monitor queries.
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96, 96);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_err() {
            return 1.0;
        }
        x as f32 / 96.0
    }
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn vector4_bytes(color: D2D1_COLOR_F) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (chunk, value) in bytes
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip([color.r, color.g, color.b, color.a])
    {
        *chunk = value.to_le_bytes();
    }
    bytes
}

fn ellipse(point: Vector2, radius: f32) -> D2D1_ELLIPSE {
    D2D1_ELLIPSE {
        point,
        radiusX: radius,
        radiusY: radius,
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}
