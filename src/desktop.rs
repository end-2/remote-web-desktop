//! Native desktop access. Construct and use this object on one dedicated worker thread.
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(not(target_os = "windows"))]
use enigo::Coordinate;
use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use font8x8::UnicodeFonts;
use image::{DynamicImage, Rgb, RgbImage, RgbaImage};
use serde::Serialize;

use crate::protocol::InputEvent;

#[cfg(target_os = "macos")]
#[path = "macos_capture.rs"]
mod macos_capture;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum CaptureBackend {
    /// ScreenCaptureKit on macOS, falling back to XCap if unavailable.
    Auto,
    /// macOS ScreenCaptureKit; report errors instead of falling back.
    Sck,
    /// Original full-resolution XCap screenshot path for comparison.
    Xcap,
}

// Exercise the vendored no-I/O state regression tests in the normal product
// test suite; Enigo's own full test suite includes real desktop input.
#[cfg(all(test, target_os = "macos"))]
#[path = "../vendor/enigo/src/macos/pointer_state.rs"]
mod macos_pointer_regression;

#[derive(Clone, Debug, Serialize)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub left: i32,
    pub top: i32,
    /// Desktop input coordinates; on macOS these are points, not Retina pixels.
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct DesktopInfo {
    pub hostname: String,
    pub platform: String,
    pub mode: String,
    pub monitors: Vec<MonitorInfo>,
}

pub struct Desktop {
    info: DesktopInfo,
    backend: Backend,
}

enum Backend {
    Native {
        monitors: Vec<xcap::Monitor>,
        input: Box<InputController<NativeDevice>>,
        capture_backend: CaptureBackend,
        #[cfg(target_os = "macos")]
        capture: Option<macos_capture::Capture>,
    },
    Demo {
        background: RgbImage,
        input: InputController<DemoDevice>,
    },
}

impl Desktop {
    pub fn new(demo: bool, capture_backend: CaptureBackend) -> Result<Self, String> {
        let mut info = DesktopInfo {
            hostname: hostname::get()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            platform: std::env::consts::OS.to_owned(),
            mode: if demo { "demo" } else { "desktop" }.to_owned(),
            monitors: Vec::new(),
        };
        if demo {
            info.monitors.push(MonitorInfo {
                id: 1,
                name: "Demo Display".to_owned(),
                left: 0,
                top: 0,
                width: 1440,
                height: 900,
            });
            return Ok(Self {
                info,
                backend: Backend::Demo {
                    background: demo_background(),
                    input: InputController::new(DemoDevice::default()),
                },
            });
        }

        #[cfg(not(target_os = "macos"))]
        if capture_backend == CaptureBackend::Sck {
            return Err("ScreenCaptureKit은 macOS에서만 사용할 수 있습니다.".into());
        }
        platform_preflight()?;
        let mut monitors = xcap::Monitor::all().map_err(|e| {
            format!("디스플레이를 찾을 수 없습니다. 로그인된 데스크톱에서 실행하세요: {e}")
        })?;
        // Stable public IDs start at 1, with the primary display first.
        monitors.sort_by_key(|m| !m.is_primary().unwrap_or(false));
        for (index, monitor) in monitors.iter().enumerate() {
            let geometry = || -> xcap::XCapResult<MonitorInfo> {
                Ok(MonitorInfo {
                    id: index as u32 + 1,
                    name: monitor.friendly_name().or_else(|_| monitor.name())?,
                    left: monitor.x()?,
                    top: monitor.y()?,
                    width: monitor.width()?,
                    height: monitor.height()?,
                })
            };
            let monitor_info = geometry().map_err(|e| format!("디스플레이 정보 조회 실패: {e}"))?;
            if monitor_info.width == 0 || monitor_info.height == 0 {
                return Err("사용할 수 없는 크기의 디스플레이입니다.".into());
            }
            info.monitors.push(monitor_info);
        }
        if monitors.is_empty() {
            return Err("연결된 디스플레이가 없습니다. 로그인된 데스크톱에서 실행하세요.".into());
        }
        let enigo = Enigo::new(&Settings {
            open_prompt_to_get_permissions: false,
            linux_delay: 0,
            ..Settings::default()
        })
        .map_err(|e| {
            format!("입력 제어 초기화 실패. macOS에서는 손쉬운 사용 권한을 확인하세요: {e}")
        })?;
        Ok(Self {
            info,
            backend: Backend::Native {
                monitors,
                input: Box::new(InputController::new(NativeDevice { enigo })),
                capture_backend,
                #[cfg(target_os = "macos")]
                capture: None,
            },
        })
    }

    pub fn info(&self) -> DesktopInfo {
        self.info.clone()
    }

    pub fn capture_raw(
        &mut self,
        monitor: u32,
        max_width: u32,
        fps: u32,
    ) -> Result<RgbaImage, String> {
        let index = self.monitor_index(monitor)?;
        if !(64..=7680).contains(&max_width) || !(1..=60).contains(&fps) {
            return Err("화면 캡처 설정이 허용 범위를 벗어났습니다.".into());
        }
        let image = match &mut self.backend {
            Backend::Native {
                monitors,
                capture_backend,
                #[cfg(target_os = "macos")]
                capture,
                ..
            } => {
                #[cfg(target_os = "macos")]
                if *capture_backend != CaptureBackend::Xcap {
                    let native = &monitors[index];
                    let display_id = native.id().map_err(|e| e.to_string())?;
                    let native_width = native.width().map_err(|e| e.to_string())?;
                    let native_height = native.height().map_err(|e| e.to_string())?;
                    let density = native.scale_factor().map_err(|e| e.to_string())? as f64;
                    let scale = (max_width as f64 / native_width as f64)
                        .min(8192.0 / native_height as f64)
                        .min(density);
                    let settings = macos_capture::CaptureSettings {
                        display_id,
                        width: ((native_width as f64 * scale).round() as u32).max(2) & !1,
                        height: ((native_height as f64 * scale).round() as u32).max(2) & !1,
                        fps,
                    };
                    let result = (|| {
                        if capture
                            .as_ref()
                            .is_none_or(|active| active.settings != settings)
                        {
                            *capture = None;
                            *capture = Some(macos_capture::Capture::start(settings)?);
                        }
                        capture.as_mut().unwrap().frame()
                    })();
                    match result {
                        Ok(image) => return Ok(image),
                        Err(error) if *capture_backend == CaptureBackend::Auto => {
                            tracing::warn!(%error, "ScreenCaptureKit unavailable; falling back to XCap");
                            *capture = None;
                            *capture_backend = CaptureBackend::Xcap;
                        }
                        Err(error) => {
                            return Err(format!("ScreenCaptureKit 화면 캡처 실패: {error}"));
                        }
                    }
                }
                #[cfg(not(target_os = "macos"))]
                let _ = capture_backend;
                monitors[index].capture_image().map_err(|e| {
                    format!("화면 캡처 실패. 화면 기록 권한과 디스플레이 연결을 확인하세요: {e}")
                })?
            }
            Backend::Demo { background, input } => {
                DynamicImage::ImageRgb8(demo_frame(background, &input.device)).into_rgba8()
            }
        };
        Ok(image)
    }

    pub fn capture_backend(&self) -> &'static str {
        match &self.backend {
            Backend::Demo { .. } => "demo",
            Backend::Native {
                capture_backend,
                #[cfg(target_os = "macos")]
                capture,
                ..
            } => {
                #[cfg(target_os = "macos")]
                if capture.is_some() {
                    return "sck";
                }
                match capture_backend {
                    CaptureBackend::Auto => {
                        #[cfg(target_os = "macos")]
                        {
                            "auto"
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            "xcap"
                        }
                    }
                    CaptureBackend::Sck => "sck",
                    CaptureBackend::Xcap => "xcap",
                }
            }
        }
    }

    pub fn capture_metrics(&self) -> (Option<u64>, Option<f64>) {
        #[cfg(target_os = "macos")]
        if let Backend::Native {
            capture: Some(capture),
            ..
        } = &self.backend
        {
            return capture.metrics();
        }
        (None, None)
    }

    /// Stop native streaming when the only client disconnects.
    pub fn stop_capture(&mut self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        if let Backend::Native { capture, .. } = &mut self.backend {
            *capture = None;
        }
        Ok(())
    }

    pub fn input(&mut self, event: &InputEvent, monitor: u32) -> Result<(), String> {
        // Releasing keys must work even if a display was removed or changed.
        if matches!(event, InputEvent::Release) {
            return self.release_all();
        }
        let index = self.monitor_index(monitor)?;
        let monitor = &self.info.monitors[index];
        match &mut self.backend {
            Backend::Native { input, .. } => input.apply(event, monitor),
            Backend::Demo { input, .. } => input.apply(event, monitor),
        }
    }

    pub fn release_all(&mut self) -> Result<(), String> {
        match &mut self.backend {
            Backend::Native { input, .. } => input.release_all(),
            Backend::Demo { input, .. } => input.release_all(),
        }
    }

    fn monitor_index(&self, id: u32) -> Result<usize, String> {
        self.info
            .monitors
            .iter()
            .position(|m| m.id == id)
            .ok_or_else(|| format!("존재하지 않는 디스플레이입니다: {id}"))
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = self.release_all();
        let _ = self.stop_capture();
    }
}

fn position(monitor: &MonitorInfo, x: f64, y: f64) -> Result<(i32, i32), String> {
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err("포인터 좌표는 0~1 범위의 유한한 수여야 합니다.".into());
    }
    // The last pixel must remain inside this monitor, including negative origins.
    Ok((
        (monitor.left as f64 + x * monitor.width.saturating_sub(1) as f64).round() as i32,
        (monitor.top as f64 + y * monitor.height.saturating_sub(1) as f64).round() as i32,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyAction {
    Raw(u16),
    Named(Key),
}

trait InputDevice {
    fn pointer(&mut self, x: i32, y: i32) -> Result<(), String>;
    fn button(&mut self, button: Button, down: bool) -> Result<(), String>;
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), String>;
    fn key(&mut self, key: KeyAction, down: bool) -> Result<(), String>;
    fn text(&mut self, text: &str) -> Result<(), String>;
}

struct NativeDevice {
    enigo: Enigo,
}

fn input_error(error: enigo::InputError) -> String {
    format!("입력 전송 실패. 호스트의 입력 권한과 키보드 배열을 확인하세요: {error}")
}

fn direction(down: bool) -> Direction {
    if down {
        Direction::Press
    } else {
        Direction::Release
    }
}

impl InputDevice for NativeDevice {
    fn pointer(&mut self, x: i32, y: i32) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            // enigo 0.6.1 normalizes absolute movement to the primary display
            // and omits MOUSEEVENTF_VIRTUALDESK. Use the established Win32 API
            // binding for virtual-desktop coordinates, including negative x/y.
            if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y) } == 0 {
                Err(format!(
                    "Windows 포인터 이동 실패: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                Ok(())
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.enigo
                .move_mouse(x, y, Coordinate::Abs)
                .map_err(input_error)
        }
    }

    fn button(&mut self, button: Button, down: bool) -> Result<(), String> {
        self.enigo
            .button(button, direction(down))
            .map_err(input_error)
    }

    fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        if dx != 0 {
            self.enigo
                .scroll(dx, Axis::Horizontal)
                .map_err(input_error)?;
        }
        if dy != 0 {
            self.enigo.scroll(dy, Axis::Vertical).map_err(input_error)?;
        }
        Ok(())
    }

    fn key(&mut self, key: KeyAction, down: bool) -> Result<(), String> {
        match key {
            KeyAction::Raw(code) => self.enigo.raw(code, direction(down)),
            KeyAction::Named(key) => self.enigo.key(key, direction(down)),
        }
        .map_err(input_error)
    }

    fn text(&mut self, text: &str) -> Result<(), String> {
        self.enigo.text(text).map_err(|e| format!("텍스트 전송 실패. 이 앱 또는 입력 방식이 Unicode 입력을 지원하지 않을 수 있습니다: {e}"))
    }
}

struct InputController<D: InputDevice> {
    device: D,
    keys: Vec<(String, KeyAction)>,
    buttons: Vec<Button>,
}

impl<D: InputDevice> InputController<D> {
    fn new(device: D) -> Self {
        Self {
            device,
            keys: Vec::new(),
            buttons: Vec::new(),
        }
    }

    fn apply(&mut self, event: &InputEvent, monitor: &MonitorInfo) -> Result<(), String> {
        match event {
            InputEvent::Pointer { x, y } => {
                let (x, y) = position(monitor, *x, *y)?;
                self.device.pointer(x, y)
            }
            InputEvent::Button { button, down, x, y } => {
                let button = match button.as_str() {
                    "left" => Button::Left,
                    "middle" => Button::Middle,
                    "right" => Button::Right,
                    _ => return Err("지원하지 않는 마우스 버튼입니다.".into()),
                };
                let (x, y) = position(monitor, *x, *y)?;
                self.device.pointer(x, y)?;
                if *down {
                    if !self.buttons.contains(&button) {
                        // Track before injection: an OS call can fail after sending an event.
                        self.buttons.push(button);
                        self.device.button(button, true)?;
                    }
                } else if self.buttons.contains(&button) {
                    self.device.button(button, false)?;
                    self.buttons.retain(|b| *b != button);
                }
                Ok(())
            }
            InputEvent::Scroll { dx, dy } => {
                if !(-20..=20).contains(dx) || !(-20..=20).contains(dy) {
                    return Err("스크롤 양이 허용 범위를 벗어났습니다.".into());
                }
                // Wire protocol: positive dy scrolls up. enigo's positive
                // vertical length scrolls down; horizontal positive is right.
                self.device.scroll(*dx, -*dy)
            }
            InputEvent::Key { code, key, down } => {
                let identity = if code.is_empty() { key } else { code };
                let held = self.keys.iter().position(|(id, _)| id == identity);
                if *down {
                    let action = if let Some(index) = held {
                        self.keys[index].1
                    } else {
                        if self.keys.len() >= 128 {
                            return Err("동시에 누를 수 있는 키 수를 초과했습니다.".into());
                        }
                        let action = resolve_key(code, key)?;
                        self.keys.push((identity.clone(), action));
                        action
                    };
                    // Repeated browser keydowns remain repeated native keydowns.
                    self.device.key(action, true)
                } else if let Some(index) = held {
                    self.device.key(self.keys[index].1, false)?;
                    self.keys.remove(index);
                    Ok(())
                } else {
                    Ok(())
                }
            }
            InputEvent::Text { text } => {
                if text.chars().count() > 2000 || text.contains('\0') {
                    return Err("텍스트는 NUL 없이 2,000자 이하여야 합니다.".into());
                }
                self.release_all()?;
                self.device.text(text)
            }
            InputEvent::Release => self.release_all(),
        }
    }

    fn release_all(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        for index in (0..self.keys.len()).rev() {
            match self.device.key(self.keys[index].1, false) {
                Ok(()) => {
                    self.keys.remove(index);
                }
                Err(error) => failures.push(error),
            }
        }
        for index in (0..self.buttons.len()).rev() {
            match self.device.button(self.buttons[index], false) {
                Ok(()) => {
                    self.buttons.remove(index);
                }
                Err(error) => failures.push(error),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

fn resolve_key(code: &str, key: &str) -> Result<KeyAction, String> {
    // Raw codes follow the host layout/IME, not the client's translated character.
    if let Some(raw) = physical_key(code) {
        return Ok(KeyAction::Raw(raw));
    }
    let named = match code {
        "Enter" | "NumpadEnter" => Key::Return,
        "Escape" => Key::Escape,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Space" => Key::Space,
        "Delete" => Key::Delete,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ShiftLeft" => Key::Shift,
        "ShiftRight" => Key::RShift,
        "ControlLeft" => Key::Control,
        "ControlRight" => Key::RControl,
        "AltLeft" | "AltRight" => Key::Alt,
        "MetaLeft" | "MetaRight" => Key::Meta,
        "CapsLock" => Key::CapsLock,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        "F13" => Key::F13,
        "F14" => Key::F14,
        "F15" => Key::F15,
        "F16" => Key::F16,
        "F17" => Key::F17,
        "F18" => Key::F18,
        "F19" => Key::F19,
        "F20" => Key::F20,
        #[cfg(not(target_os = "macos"))]
        "Insert" => Key::Insert,
        #[cfg(not(target_os = "macos"))]
        "PrintScreen" => Key::PrintScr,
        #[cfg(not(target_os = "macos"))]
        "Pause" => Key::Pause,
        #[cfg(not(target_os = "macos"))]
        "NumLock" => Key::Numlock,
        _ => {
            let mut chars = key.chars();
            if let (Some(character), None) = (chars.next(), chars.next())
                && !character.is_control()
            {
                return Ok(KeyAction::Named(Key::Unicode(character)));
            }
            return Err(format!(
                "지원하지 않는 키입니다: {code}. IME 입력은 텍스트 전송을 사용하세요."
            ));
        }
    };
    Ok(KeyAction::Named(named))
}

/// (DOM code, macOS virtual key, PC set-1 scan code). X11 evdev = set-1 + 8
/// for these non-extended keys. Named keys cover the platform-specific remainder.
const PHYSICAL_KEYS: &[(&str, u16, u16)] = &[
    ("KeyA", 0, 30),
    ("KeyB", 11, 48),
    ("KeyC", 8, 46),
    ("KeyD", 2, 32),
    ("KeyE", 14, 18),
    ("KeyF", 3, 33),
    ("KeyG", 5, 34),
    ("KeyH", 4, 35),
    ("KeyI", 34, 23),
    ("KeyJ", 38, 36),
    ("KeyK", 40, 37),
    ("KeyL", 37, 38),
    ("KeyM", 46, 50),
    ("KeyN", 45, 49),
    ("KeyO", 31, 24),
    ("KeyP", 35, 25),
    ("KeyQ", 12, 16),
    ("KeyR", 15, 19),
    ("KeyS", 1, 31),
    ("KeyT", 17, 20),
    ("KeyU", 32, 22),
    ("KeyV", 9, 47),
    ("KeyW", 13, 17),
    ("KeyX", 7, 45),
    ("KeyY", 16, 21),
    ("KeyZ", 6, 44),
    ("Digit1", 18, 2),
    ("Digit2", 19, 3),
    ("Digit3", 20, 4),
    ("Digit4", 21, 5),
    ("Digit5", 23, 6),
    ("Digit6", 22, 7),
    ("Digit7", 26, 8),
    ("Digit8", 28, 9),
    ("Digit9", 25, 10),
    ("Digit0", 29, 11),
    ("Minus", 27, 12),
    ("Equal", 24, 13),
    ("BracketLeft", 33, 26),
    ("BracketRight", 30, 27),
    ("Backslash", 42, 43),
    ("Semicolon", 41, 39),
    ("Quote", 39, 40),
    ("Backquote", 50, 41),
    ("Comma", 43, 51),
    ("Period", 47, 52),
    ("Slash", 44, 53),
    ("IntlBackslash", 10, 86),
    ("Numpad0", 82, 82),
    ("Numpad1", 83, 79),
    ("Numpad2", 84, 80),
    ("Numpad3", 85, 81),
    ("Numpad4", 86, 75),
    ("Numpad5", 87, 76),
    ("Numpad6", 88, 77),
    ("Numpad7", 89, 71),
    ("Numpad8", 91, 72),
    ("Numpad9", 92, 73),
    ("NumpadDecimal", 65, 83),
    ("NumpadAdd", 69, 78),
    ("NumpadSubtract", 78, 74),
    ("NumpadMultiply", 67, 55),
];

fn physical_key(code: &str) -> Option<u16> {
    // The PC extended keys are not part of the simple evdev = scan+8 table.
    let extended = match code {
        "AltRight" => Some((61, 56, 108)),
        "MetaRight" => Some((54, 92, 134)),
        "NumpadEnter" => Some((76, 28, 104)),
        "NumpadDivide" => Some((75, 53, 106)),
        _ => None,
    };
    if let Some(entry) = extended {
        #[cfg(target_os = "macos")]
        {
            return Some(entry.0);
        }
        #[cfg(target_os = "windows")]
        {
            return Some(entry.1 | enigo::EXT);
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            return Some(entry.2);
        }
    }
    PHYSICAL_KEYS
        .iter()
        .find(|entry| entry.0 == code)
        .map(|entry| {
            #[cfg(target_os = "macos")]
            {
                entry.1
            }
            #[cfg(target_os = "windows")]
            {
                entry.2
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                entry.2 + 8
            }
        })
}

pub fn request_permissions() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::request_permissions()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("--request-permissions는 macOS에서만 지원합니다.".into())
    }
}

fn platform_preflight() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    macos::preflight()?;
    #[cfg(target_os = "windows")]
    {
        // Set before xcap/enigo are initialized so scaled monitors share coordinates.
        let _ = enigo::set_dpi_awareness();
    }
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland")
        {
            return Err(
                "Wayland 세션은 지원하지 않습니다. X11/Xorg로 로그인한 뒤 실행하세요.".into(),
            );
        }
        if std::env::var_os("DISPLAY").is_none() {
            return Err("DISPLAY가 없습니다. 로그인된 X11 데스크톱 세션에서 실행하세요.".into());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: *const c_void;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }

    pub(super) fn request_permissions() -> Result<(), String> {
        let screen = unsafe { CGPreflightScreenCaptureAccess() };
        let input = unsafe { AXIsProcessTrusted() };
        if screen && input {
            return Ok(());
        }
        eprintln!(
            "macOS 권한 요청: 화면 기록은 화면 공유에, 손쉬운 사용은 마우스·키보드 제어에 필요합니다.\n\
             시스템 설정 > 개인정보 보호 및 보안에서 서버를 실행하는 앱(또는 Terminal/IDE)을 허용하세요.\n\
             권한 변경 후에는 서버를 다시 시작하세요. 자동으로 창을 열지 않으려면 --no-permission-prompts를 사용하세요."
        );
        // These APIs ask the user; they do not grant or reset any permissions.
        unsafe {
            if !screen {
                CGRequestScreenCaptureAccess();
            }
            if !input {
                let key = kAXTrustedCheckOptionPrompt;
                let value = kCFBooleanTrue;
                // Keys and values are static framework constants and outlive this dictionary.
                let options = CFDictionaryCreate(
                    std::ptr::null(),
                    &key,
                    &value,
                    1,
                    std::ptr::null(),
                    std::ptr::null(),
                );
                if options.is_null() {
                    return Err("권한 요청 옵션을 만들 수 없습니다.".into());
                }
                AXIsProcessTrustedWithOptions(options);
                CFRelease(options);
            }
        }
        // A previous denial can suppress the OS prompt. Open the missing
        // permission's pane as well, once per launch. If both are missing,
        // start with Screen Recording; Accessibility is in the same sidebar.
        let screen = unsafe { CGPreflightScreenCaptureAccess() };
        let input = unsafe { AXIsProcessTrusted() };
        if !screen || !input {
            tracing::warn!(
                screen_recording = screen,
                accessibility = input,
                "macOS permissions pending; allow access in System Settings and restart the server"
            );
            let pane = if !screen {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            } else {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            };
            let output = std::process::Command::new("/usr/bin/open")
                .arg(pane)
                .output()
                .map_err(|error| format!("macOS 권한 설정을 열 수 없습니다: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "macOS 권한 설정을 열 수 없습니다. 시스템 설정 > 개인정보 보호 및 보안에서 직접 허용하세요: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
        Ok(())
    }

    pub(super) fn preflight() -> Result<(), String> {
        // Read-only preflight: no screen recording or input event is performed.
        let screen = unsafe { CGPreflightScreenCaptureAccess() };
        let input = unsafe { AXIsProcessTrusted() };
        if !screen || !input {
            let mut missing = Vec::new();
            if !screen {
                missing.push("화면 및 시스템 오디오 녹음(화면 기록)");
            }
            if !input {
                missing.push("손쉬운 사용");
            }
            return Err(format!(
                "macOS 권한 필요: {}. 시스템 설정 > 개인정보 보호 및 보안에서 서버를 실행하는 Terminal/앱에 권한을 허용한 뒤 서버를 다시 시작하세요.",
                missing.join(", ")
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
struct DemoDevice {
    pointer: (i32, i32),
    events: VecDeque<String>,
    text: String,
}

impl DemoDevice {
    fn record(&mut self, value: String) {
        self.events.push_front(value);
        self.events.truncate(6);
    }
}

impl InputDevice for DemoDevice {
    fn pointer(&mut self, x: i32, y: i32) -> Result<(), String> {
        self.pointer = (x, y);
        Ok(())
    }
    fn button(&mut self, button: Button, down: bool) -> Result<(), String> {
        self.record(format!(
            "{button:?} {}",
            if down { "pressed" } else { "released" }
        ));
        Ok(())
    }
    fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        self.record(format!("Scroll  x:{dx:+}  y:{dy:+}"));
        Ok(())
    }
    fn key(&mut self, key: KeyAction, down: bool) -> Result<(), String> {
        self.record(format!("{key:?} {}", if down { "down" } else { "up" }));
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<(), String> {
        self.text = text.chars().take(52).collect();
        self.record(format!(
            "Received {} Unicode characters",
            text.chars().count()
        ));
        Ok(())
    }
}

fn rect(image: &mut RgbImage, x: u32, y: u32, width: u32, height: u32, color: [u8; 3]) {
    for yy in y..y.saturating_add(height).min(image.height()) {
        for xx in x..x.saturating_add(width).min(image.width()) {
            image.put_pixel(xx, yy, Rgb(color));
        }
    }
}

fn text(image: &mut RgbImage, x: u32, y: u32, value: &str, scale: u32, color: [u8; 3]) {
    let mut x = x;
    for character in value.chars() {
        let glyph = font8x8::BASIC_FONTS
            .get(character)
            .or_else(|| font8x8::BASIC_FONTS.get('?'))
            .unwrap();
        for (row, bits) in glyph.iter().enumerate() {
            for bit in 0..8 {
                if bits & (1 << bit) != 0 {
                    rect(
                        image,
                        x + bit * scale,
                        y + row as u32 * scale,
                        scale,
                        scale,
                        color,
                    );
                }
            }
        }
        x += 9 * scale;
        if x >= image.width() {
            break;
        }
    }
}

fn demo_background() -> RgbImage {
    let mut image = RgbImage::from_fn(1440, 900, |x, y| {
        let glow = ((x as f32 / 1440.0) * (1.0 - y as f32 / 1100.0) * 28.0) as u8;
        Rgb([15 + glow / 3, 22 + glow / 2, 34 + glow])
    });
    rect(&mut image, 0, 0, 1440, 40, [19, 25, 35]);
    text(
        &mut image,
        24,
        13,
        "ORBIT  /  Remote workspace",
        2,
        [208, 222, 232],
    );
    text(&mut image, 1190, 14, "DEMO SESSION", 1, [130, 214, 169]);
    // A rendered desktop window with a quiet sidebar and a functional input log.
    rect(&mut image, 98, 116, 1252, 680, [9, 14, 23]);
    rect(&mut image, 90, 108, 1252, 680, [31, 40, 55]);
    rect(&mut image, 90, 108, 1252, 46, [43, 53, 69]);
    for (i, color) in [[235, 115, 115], [230, 189, 96], [123, 195, 152]]
        .into_iter()
        .enumerate()
    {
        rect(&mut image, 111 + i as u32 * 24, 125, 10, 10, color);
    }
    text(&mut image, 539, 125, "REMOTE DESKTOP", 2, [194, 208, 222]);
    rect(&mut image, 90, 154, 248, 634, [25, 33, 47]);
    text(&mut image, 116, 192, "WORKSPACE", 2, [139, 158, 179]);
    rect(&mut image, 107, 239, 214, 46, [47, 67, 76]);
    text(&mut image, 124, 254, "Overview", 2, [157, 229, 187]);
    text(&mut image, 124, 318, "Documents", 2, [148, 164, 184]);
    text(&mut image, 124, 380, "Activity", 2, [148, 164, 184]);
    text(&mut image, 116, 710, "HOST ONLINE", 1, [144, 216, 170]);
    text(
        &mut image,
        378,
        197,
        "Your desktop, anywhere.",
        3,
        [228, 235, 244],
    );
    text(
        &mut image,
        380,
        249,
        "A live preview of your remote connection.",
        2,
        [146, 165, 186],
    );
    rect(&mut image, 378, 305, 916, 124, [39, 57, 65]);
    text(
        &mut image,
        402,
        329,
        "CONNECTION ESTABLISHED",
        2,
        [149, 222, 177],
    );
    text(
        &mut image,
        402,
        369,
        "Move, click, scroll or type to test your controls.",
        1,
        [190, 208, 216],
    );
    text(&mut image, 378, 470, "INPUT ACTIVITY", 2, [196, 210, 226]);
    text(&mut image, 860, 470, "TEXT TRANSFER", 2, [196, 210, 226]);
    text(
        &mut image,
        860,
        523,
        "Send Unicode text from",
        2,
        [145, 164, 184],
    );
    text(
        &mut image,
        860,
        553,
        "the session toolbar.",
        2,
        [145, 164, 184],
    );
    text(
        &mut image,
        378,
        749,
        "Demo mode: no host screen is captured and no real input is sent.",
        1,
        [129, 148, 169],
    );
    rect(&mut image, 544, 835, 352, 48, [39, 52, 68]);
    text(
        &mut image,
        570,
        851,
        "FILES    WEB    TERMINAL",
        1,
        [203, 219, 230],
    );
    image
}

fn demo_frame(background: &RgbImage, device: &DemoDevice) -> RgbImage {
    let mut image = background.clone();
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    text(
        &mut image,
        952,
        14,
        &format!(
            "{:02}:{:02}:{:02} UTC",
            seconds / 3600 % 24,
            seconds / 60 % 60,
            seconds % 60
        ),
        1,
        [183, 201, 217],
    );
    if device.events.is_empty() {
        text(
            &mut image,
            380,
            523,
            "Waiting for your first input...",
            1,
            [134, 156, 179],
        );
    }
    for (index, event) in device.events.iter().enumerate() {
        text(
            &mut image,
            380,
            523 + index as u32 * 29,
            &event.chars().take(47).collect::<String>(),
            1,
            [171, 199, 218],
        );
    }
    if !device.text.is_empty() {
        let preview: String = device.text.chars().take(24).collect();
        text(&mut image, 860, 620, &preview, 1, [165, 226, 186]);
    }
    text(
        &mut image,
        24,
        865,
        &format!("POINTER {:04}, {:04}", device.pointer.0, device.pointer.1),
        1,
        [149, 170, 188],
    );
    let x = device.pointer.0.max(0) as u32;
    let y = device.pointer.1.max(0) as u32;
    for row in 0..19 {
        rect(&mut image, x, y + row, row / 2 + 2, 1, [12, 22, 33]);
        if row > 1 {
            rect(
                &mut image,
                x + 1,
                y + row,
                (row / 2).max(1),
                1,
                [230, 246, 239],
            );
        }
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor() -> MonitorInfo {
        MonitorInfo {
            id: 1,
            name: "Retina".into(),
            left: -1440,
            top: -100,
            width: 1440,
            height: 900,
        }
    }

    #[derive(Default)]
    struct FakeDevice {
        calls: Vec<String>,
        fail_release: bool,
    }
    impl InputDevice for FakeDevice {
        fn pointer(&mut self, x: i32, y: i32) -> Result<(), String> {
            self.calls.push(format!("pointer:{x},{y}"));
            Ok(())
        }
        fn button(&mut self, button: Button, down: bool) -> Result<(), String> {
            self.calls.push(format!("button:{button:?}:{down}"));
            Ok(())
        }
        fn scroll(&mut self, dx: i32, dy: i32) -> Result<(), String> {
            self.calls.push(format!("scroll:{dx},{dy}"));
            Ok(())
        }
        fn key(&mut self, key: KeyAction, down: bool) -> Result<(), String> {
            self.calls.push(format!("key:{key:?}:{down}"));
            if !down && self.fail_release {
                Err("release failed".into())
            } else {
                Ok(())
            }
        }
        fn text(&mut self, value: &str) -> Result<(), String> {
            self.calls.push(format!("text:{value}"));
            Ok(())
        }
    }

    #[test]
    fn maps_normalized_positions_in_logical_monitor_space() {
        assert_eq!(position(&monitor(), 0.0, 0.0).unwrap(), (-1440, -100));
        assert_eq!(position(&monitor(), 1.0, 1.0).unwrap(), (-1, 799));
        assert_eq!(position(&monitor(), 0.5, 0.5).unwrap(), (-721, 350));
        assert!(position(&monitor(), f64::NAN, 0.0).is_err());
        assert!(position(&monitor(), 1.1, 0.0).is_err());
    }

    #[test]
    fn key_repeats_and_changed_keyup_character_release_the_same_key() {
        let mut input = InputController::new(FakeDevice::default());
        let down = InputEvent::Key {
            code: "KeyA".into(),
            key: "A".into(),
            down: true,
        };
        input.apply(&down, &monitor()).unwrap();
        input.apply(&down, &monitor()).unwrap();
        input
            .apply(
                &InputEvent::Key {
                    code: "KeyA".into(),
                    key: "a".into(),
                    down: false,
                },
                &monitor(),
            )
            .unwrap();
        assert_eq!(input.device.calls.len(), 3);
        assert_eq!(input.device.calls[0], input.device.calls[1]);
        assert_eq!(
            input.device.calls[2],
            input.device.calls[0].replace("true", "false")
        );
        assert!(input.keys.is_empty());
    }

    #[test]
    fn release_attempts_every_held_input_and_keeps_failed_keys_for_retry() {
        let mut input = InputController::new(FakeDevice::default());
        input
            .apply(
                &InputEvent::Key {
                    code: "ControlLeft".into(),
                    key: "Control".into(),
                    down: true,
                },
                &monitor(),
            )
            .unwrap();
        input
            .apply(
                &InputEvent::Key {
                    code: "KeyA".into(),
                    key: "a".into(),
                    down: true,
                },
                &monitor(),
            )
            .unwrap();
        input
            .apply(
                &InputEvent::Button {
                    button: "left".into(),
                    down: true,
                    x: 0.5,
                    y: 0.5,
                },
                &monitor(),
            )
            .unwrap();
        input.device.fail_release = true;
        assert!(input.release_all().is_err());
        assert_eq!(input.keys.len(), 2);
        assert!(input.buttons.is_empty());
        assert!(
            input
                .device
                .calls
                .last()
                .unwrap()
                .contains("button:Left:false")
        );
        input.device.fail_release = false;
        input.release_all().unwrap();
        assert!(input.keys.is_empty());
    }

    #[test]
    fn unicode_text_releases_modifiers_and_never_reads_clipboard() {
        let mut input = InputController::new(FakeDevice::default());
        input
            .apply(
                &InputEvent::Key {
                    code: "ControlLeft".into(),
                    key: "Control".into(),
                    down: true,
                },
                &monitor(),
            )
            .unwrap();
        input
            .apply(
                &InputEvent::Text {
                    text: "안녕하세요 👋".into(),
                },
                &monitor(),
            )
            .unwrap();
        assert!(input.keys.is_empty());
        assert!(input.device.calls[1].ends_with(":false"));
        assert_eq!(input.device.calls[2], "text:안녕하세요 👋");
    }

    #[test]
    fn browser_scroll_direction_maps_to_native_wheel_semantics() {
        let mut input = InputController::new(FakeDevice::default());
        input
            .apply(&InputEvent::Scroll { dx: 2, dy: 3 }, &monitor())
            .unwrap();
        input
            .apply(&InputEvent::Scroll { dx: -2, dy: -3 }, &monitor())
            .unwrap();
        assert_eq!(input.device.calls, ["scroll:2,-3", "scroll:-2,3"]);
    }

    #[test]
    fn demo_captures_raw_frame_and_receives_input_without_native_access() {
        let mut desktop = Desktop::new(true, CaptureBackend::Auto).unwrap();
        desktop
            .input(&InputEvent::Pointer { x: 0.5, y: 0.5 }, 1)
            .unwrap();
        desktop
            .input(
                &InputEvent::Text {
                    text: "Hello".into(),
                },
                1,
            )
            .unwrap();
        let frame = desktop.capture_raw(1, 1600, 30).unwrap();
        assert_eq!((frame.width(), frame.height()), (1440, 900));
        assert_eq!(desktop.info().mode, "demo");
        assert!(desktop.capture_raw(9, 1600, 30).is_err());
    }
}
