#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use ime_engine::EngineEvent;
use ime_fcitx5::keygrab::{self, GrabbedKey, KeyGrabber};
use ime_fcitx5::ImeServer;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use x11_dl::xlib;

/// App / tray icon (embedded for the tray and the desktop entry).
const ICON_PNG: &[u8] = include_bytes!("../icons/256x256.png");

/// Prebuilt fcitx5 addon (built by scripts/build-addon.sh). Embedded so the
/// app can install it to the user's fcitx5 without sudo or a compiler.
const ADDON_SO: &[u8] = include_bytes!("../addons/librustime.so");

/// Whether the IME is in Chinese (pinyin) mode. Toggled by Ctrl+Space.
static IME_ACTIVE: AtomicBool = AtomicBool::new(true);

/// Thread-safe wrapper for the X11 Display pointer.
#[derive(Clone)]
struct DisplayHandle(*mut xlib::Display);

unsafe impl Send for DisplayHandle {}
unsafe impl Sync for DisplayHandle {}

impl DisplayHandle {
    fn new() -> Self {
        let display: *mut xlib::Display = {
            let xl = xlib::Xlib::open().ok();
            let d = xl.and_then(|x| {
                let ptr = unsafe { (x.XOpenDisplay)(std::ptr::null()) };
                if ptr.is_null() {
                    None
                } else {
                    Some(ptr)
                }
            });
            d.unwrap_or(std::ptr::null_mut())
        };
        Self(display)
    }

    fn as_ptr(&self) -> *mut xlib::Display {
        self.0
    }

    fn is_open(&self) -> bool {
        !self.0.is_null()
    }
}

/// Shared state: IME server + optional X11 display for XIM commit.
struct AppState {
    server: ImeServer,
    display: DisplayHandle,
    settings: std::sync::Mutex<Settings>,
}

#[derive(serde::Serialize, Clone)]
struct CandidateView {
    text: String,
    pinyin: String,
    selected: bool,
}

#[derive(serde::Serialize, Clone)]
struct PreeditView {
    raw: String,
    is_valid_pinyin: bool,
    candidates: Vec<CandidateView>,
    page: usize,
    total_pages: usize,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
#[serde(default)]
struct Settings {
    candidate_count: usize,
    prediction: bool,
    learn: bool,
    autostart: bool,
    theme: String,
    font_size: u32,
    /// How to switch Chinese/English: "ctrl_space" or "shift".
    toggle_key: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            candidate_count: ime_engine::engine::MAX_CANDIDATES,
            prediction: true,
            learn: true,
            autostart: true,
            theme: "dark".to_string(),
            font_size: 16,
            toggle_key: "ctrl_space".to_string(),
        }
    }
}

fn config_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.config")))
        .unwrap_or_else(|| "/tmp".to_string());
    std::path::PathBuf::from(base).join("rust-ime")
}

fn config_path() -> std::path::PathBuf {
    config_dir().join("config.json")
}

fn user_dict_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.local/share")))
        .unwrap_or_else(|| "/tmp".to_string());
    std::path::PathBuf::from(base).join("rust-ime/udict.txt")
}

fn load_settings() -> Settings {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store_settings(s: &Settings) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(config_path(), json);
    }
}

/// Apply settings to the engine, autostart and candidate window.
fn apply_settings(app: &AppHandle, server: &ImeServer, s: &Settings) {
    server.apply_settings(s.candidate_count, s.prediction, s.learn);
    set_autostart(s.autostart);
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.emit("settings", s.clone());
    }
}

/// Create/remove the XDG autostart entry.
fn set_autostart(on: bool) {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let path = format!("{home}/.config/autostart/rust-ime.desktop");
    if on {
        if let Ok(exe) = std::env::current_exe() {
            let dir = format!("{home}/.config/autostart");
            let _ = std::fs::create_dir_all(&dir);
            let content = format!(
                "[Desktop Entry]\nType=Application\nName=rust-ime\nComment=Pinyin input method (Rust + Tauri)\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
                exe.display()
            );
            if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
                let _ = std::fs::write(&path, content);
            }
        }
    } else {
        let _ = std::fs::remove_file(&path);
    }
}

#[derive(serde::Serialize)]
struct Stats {
    system_words: usize,
    user_words: usize,
    user_dict_path: String,
    version: String,
}

#[derive(serde::Serialize)]
struct SystemStatus {
    session_type: String,
    display: String,
    x11_ok: bool,
    fcitx5_running: bool,
    fcitx5_addon_installed: bool,
    fcitx5_im_registered: bool,
    autostart_enabled: bool,
    /// "standalone" (XRecord) or "fcitx5" (plugin active).
    mode: String,
}

fn process_running(name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for e in entries.flatten() {
        let comm = e.path().join("comm");
        if let Ok(s) = std::fs::read_to_string(comm) {
            if s.trim() == name {
                return true;
            }
        }
    }
    false
}

fn any_exists(paths: &[&str]) -> bool {
    paths.iter().any(|p| std::path::Path::new(p).exists())
}

/// Detect how the IME is wired into the running system.
#[tauri::command]
fn get_system_status(state: State<'_, AppState>) -> SystemStatus {
    let home = std::env::var("HOME").unwrap_or_default();
    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
    let display = std::env::var("DISPLAY").unwrap_or_default();

    let addon_installed = any_exists(&[
        "/usr/share/fcitx5/addon/rustime.conf",
        &format!("{home}/.local/share/fcitx5/addon/rustime.conf"),
    ]) && any_exists(&[
        "/usr/lib/x86_64-linux-gnu/fcitx5/librustime.so",
        "/usr/lib/fcitx5/librustime.so",
        &format!("{home}/.local/lib/fcitx5/librustime.so"),
    ]);

    let im_registered = any_exists(&[
        "/usr/share/fcitx5/inputmethod/rust-ime.conf",
        &format!("{home}/.local/share/fcitx5/inputmethod/rust-ime.conf"),
    ]);

    let fcitx5_running = process_running("fcitx5");
    let autostart_enabled =
        std::path::Path::new(&format!("{home}/.config/autostart/rust-ime.desktop")).exists();

    let mode = if addon_installed && im_registered && fcitx5_running {
        "fcitx5"
    } else {
        "standalone"
    };

    SystemStatus {
        session_type,
        display: display.clone(),
        x11_ok: state.display.is_open(),
        fcitx5_running,
        fcitx5_addon_installed: addon_installed,
        fcitx5_im_registered: im_registered,
        autostart_enabled,
        mode: mode.to_string(),
    }
}

/// Start or stop fcitx5 (used when the two IMEs conflict).
#[tauri::command]
fn set_fcitx5(enabled: bool) -> Result<(), String> {
    let (program, args): (&str, &[&str]) = if enabled {
        ("fcitx5", &["-d", "-r"])
    } else {
        ("fcitx5-remote", &["-e"])
    };
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map_err(|e| format!("无法执行 {program}: {e}"))?;
    Ok(())
}

// ── fcitx5 addon install (user-level, no sudo) ──────────────────────────

fn fcitx5_user_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    let home = std::env::var("HOME").unwrap_or_default();
    (
        std::path::PathBuf::from(format!("{home}/.local/share/fcitx5")),
        std::path::PathBuf::from(format!("{home}/.local/lib/fcitx5")),
    )
}

/// Whether fcitx5 has our addon installed in the user's data dir.
fn fcitx5_addon_installed() -> bool {
    let (data, lib) = fcitx5_user_paths();
    data.join("addon/rustime.conf").exists()
        && data.join("inputmethod/rust-ime.conf").exists()
        && lib.join("librustime.so").exists()
}

/// The addon is active when installed and fcitx5 is running.
fn fcitx5_addon_active() -> bool {
    fcitx5_addon_installed() && process_running("fcitx5")
}

/// Insert rust-ime into the fcitx5 profile and make it the default IM.
fn update_fcitx5_profile() -> std::io::Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = format!("{home}/.config/fcitx5");
    std::fs::create_dir_all(&dir)?;
    let path = format!("{dir}/profile");
    let mut s = std::fs::read_to_string(&path).unwrap_or_default();
    if s.trim().is_empty() {
        s = "[Groups/0]\nName=默认\nDefault Layout=us\nDefaultIM=keyboard-us\n\n[GroupOrder]\n0=默认\n"
            .to_string();
    }
    let _ = std::fs::write(format!("{path}.bak-rustime"), &s);

    if !s.contains("Name=rust-ime") {
        let mut max_idx: i32 = -1;
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("[Groups/0/Items/") {
                if let Some(n) = rest.split(']').next().and_then(|n| n.parse().ok()) {
                    max_idx = max_idx.max(n);
                }
            }
        }
        let item = format!(
            "\n[Groups/0/Items/{}]\nName=rust-ime\nLayout=\n",
            max_idx + 1
        );
        match s.find("[GroupOrder]") {
            Some(pos) => s.insert_str(pos, &item),
            None => s.push_str(&item),
        }
    }

    // Set DefaultIM=rust-ime inside the first group block.
    if let Some(start) = s.find("[Groups/0]") {
        if let Some(rel) = s[start..].find("DefaultIM=") {
            let abs = start + rel;
            if let Some(len) = s[abs..].find('\n') {
                s.replace_range(abs..abs + len, "DefaultIM=rust-ime");
            }
        }
    }

    std::fs::write(&path, s)
}

/// Install the embedded addon into the user's fcitx5 and restart it.
fn do_install_fcitx5_addon() -> Result<String, String> {
    let (data, lib) = fcitx5_user_paths();
    let addon_dir = data.join("addon");
    let im_dir = data.join("inputmethod");
    std::fs::create_dir_all(&addon_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&im_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&lib).map_err(|e| e.to_string())?;

    std::fs::write(lib.join("librustime.so"), ADDON_SO)
        .map_err(|e| format!("写入插件失败: {e}"))?;

    let conf = format!(
        "[Addon]\nName=rust-ime\nComment=Rust pinyin input method\nCategory=InputMethod\nVersion={}\nLibrary={}\nType=SharedLibrary\nOnDemand=True\nConfigurable=False\n\n[Addon/Dependencies]\n0=core\n",
        env!("CARGO_PKG_VERSION"),
        lib.join("librustime").display()
    );
    std::fs::write(addon_dir.join("rustime.conf"), conf)
        .map_err(|e| format!("写入 addon 配置失败: {e}"))?;

    let im = "[InputMethod]\nName=rust-ime\nIcon=fcitx-pinyin\nLabel=理\nLangCode=zh_CN\nAddon=rustime\nConfigurable=False\n";
    std::fs::write(im_dir.join("rust-ime.conf"), im)
        .map_err(|e| format!("写入输入法配置失败: {e}"))?;

    // fcitx5 rewrites its profile on exit, so stop it BEFORE editing the
    // profile, otherwise our change is overwritten by the old instance.
    let _ = std::process::Command::new("fcitx5-remote").arg("-e").status();
    for _ in 0..30 {
        if !process_running("fcitx5") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    update_fcitx5_profile().map_err(|e| format!("更新 fcitx5 配置失败: {e}"))?;

    // Start fcitx5 again so it picks up the addon.
    let _ = std::process::Command::new("fcitx5").args(["-d", "-r"]).spawn();
    Ok("已安装 fcitx5 插件，并设为默认输入法".to_string())
}

#[tauri::command]
fn install_fcitx5_addon() -> Result<String, String> {
    do_install_fcitx5_addon()
}

/// Remove the addon from the user's fcitx5.
fn do_uninstall_fcitx5_addon() -> Result<String, String> {
    let (data, lib) = fcitx5_user_paths();
    // Stop first so fcitx5 does not rewrite the profile we restore.
    let _ = std::process::Command::new("fcitx5-remote").arg("-e").status();
    for _ in 0..30 {
        if !process_running("fcitx5") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    let _ = std::fs::remove_file(data.join("addon/rustime.conf"));
    let _ = std::fs::remove_file(data.join("inputmethod/rust-ime.conf"));
    let _ = std::fs::remove_file(lib.join("librustime.so"));

    // Restore the profile snapshot taken at install time.
    let home = std::env::var("HOME").unwrap_or_default();
    let profile = format!("{home}/.config/fcitx5/profile");
    let backup = format!("{profile}.bak-rustime");
    if std::path::Path::new(&backup).exists() {
        let _ = std::fs::copy(&backup, &profile);
    }

    let _ = std::process::Command::new("fcitx5").args(["-d", "-r"]).spawn();
    Ok("已卸载 fcitx5 插件".to_string())
}

#[tauri::command]
fn uninstall_fcitx5_addon() -> Result<String, String> {
    do_uninstall_fcitx5_addon()
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<(), String> {
    store_settings(&settings);
    *state.settings.lock().unwrap() = settings.clone();
    apply_settings(&app, &state.server, &settings);
    Ok(())
}

#[tauri::command]
fn get_stats(state: State<'_, AppState>) -> Stats {
    let (system_words, user_words) = state.server.dict_stats();
    Stats {
        system_words,
        user_words,
        user_dict_path: user_dict_path().display().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

#[tauri::command]
fn clear_user_dict(state: State<'_, AppState>) -> Result<(), String> {
    state.server.clear_user_dict();
    Ok(())
}

#[derive(serde::Serialize)]
struct UserWord {
    word: String,
    freq: f64,
}

#[tauri::command]
fn list_user_words(state: State<'_, AppState>, limit: usize) -> Vec<UserWord> {
    state
        .server
        .user_words(limit)
        .into_iter()
        .map(|(word, freq)| UserWord { word, freq })
        .collect()
}

/// Export the user dictionary to a user-visible file.
#[tauri::command]
fn export_user_dict() -> Result<String, String> {
    let src = user_dict_path();
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    let dst = std::path::PathBuf::from(format!("{home}/rust-ime-用户词库.txt"));
    if !src.exists() {
        // Nothing learned yet: write an empty, valid file.
        let _ = std::fs::create_dir_all(src.parent().unwrap_or(std::path::Path::new(".")));
        let _ = std::fs::write(&src, "");
    }
    std::fs::copy(&src, &dst).map_err(|e| format!("export failed: {e}"))?;
    Ok(dst.display().to_string())
}

#[tauri::command]
fn open_settings(app: AppHandle) -> Result<(), String> {
    show_settings(&app);
    Ok(())
}

/// Show and focus the settings window.
fn show_settings(app: &AppHandle) {
    // The always-on-top candidate strip would cover the settings window and
    // swallow clicks: dismiss it first.
    if let Some(state) = app.try_state::<AppState>() {
        let _ = state.server.reset();
        LEAKED_CHARS.store(0, Ordering::SeqCst);
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
    }
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Install a user-level desktop entry + icon so the app appears in the
/// system application menu with its logo.
fn install_desktop_entry() {
    let (Ok(home), Ok(exe)) = (
        std::env::var("HOME"),
        std::env::current_exe(),
    ) else {
        return;
    };

    let icon_dir = format!("{home}/.local/share/icons/hicolor/256x256/apps");
    let _ = std::fs::create_dir_all(&icon_dir);
    let _ = std::fs::write(format!("{icon_dir}/rust-ime.png"), ICON_PNG);

    let app_dir = format!("{home}/.local/share/applications");
    let _ = std::fs::create_dir_all(&app_dir);
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=rust-ime 输入法设置\nComment=纯 Rust 拼音输入法设置中心\nExec=\"{}\" --settings\nIcon=rust-ime\nTerminal=false\nCategories=Utility;Settings;\n",
        exe.display()
    );
    let _ = std::fs::write(format!("{app_dir}/rust-ime.desktop"), content);
}

fn snapshot(server: &ImeServer) -> PreeditView {
    let st = server.state();
    PreeditView {
        is_valid_pinyin: st.is_valid_pinyin,
        raw: st.raw.clone(),
        // Vertical list: show all candidates (engine caps at 10).
        candidates: st
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| CandidateView {
                text: c.text.clone(),
                pinyin: c.pinyin.clone(),
                selected: i == st.selected,
            })
            .collect(),
        page: 0,
        total_pages: usize::from(!st.candidates.is_empty()),
    }
}

/// The X11 window that had keyboard focus before the candidate window showed.
/// Restored before injecting text so keystrokes reach the real app, not our
/// always-on-top candidate strip.
static PREV_FOCUS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn emit_preedit(app: &AppHandle, server: &ImeServer) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let snap = snapshot(server);
    // Keep the strip visible for contextual predictions too (raw empty but
    // candidates present after a commit).
    let active = !snap.raw.is_empty() || !snap.candidates.is_empty();

    if active {
        // Snapshot the focused window BEFORE we show our always-on-top strip.
        let focus = keygrab::get_input_focus().unwrap_or(0) as u64;
        if focus != 0 && PREV_FOCUS.load(Ordering::SeqCst) == 0 {
            PREV_FOCUS.store(focus, Ordering::SeqCst);
        }

        // Vertical list: grow with the candidate count (max 10).
        let n = snap.candidates.len();
        let height = if n == 0 {
            110.0
        } else {
            (74.0 + n as f64 * 36.0).min(470.0)
        };
        let _ = win.set_size(tauri::LogicalSize::new(520.0, height));
        // Place it just below-right of the pointer.
        match keygrab::pointer_pos() {
            Some((x, y)) => {
                if let Err(e) = win.set_position(tauri::PhysicalPosition::new(x + 8, y + 24)) {
                    eprintln!("[rust-ime] set_position failed: {e}");
                }
            }
            None => eprintln!("[rust-ime] pointer_pos returned None"),
        }
        // Show FIRST: a hidden webview drops emitted events.
        let _ = win.show();

        // Do not let the strip keep focus (it would swallow injected text).
        let prev = PREV_FOCUS.load(Ordering::SeqCst);
        if prev != 0 {
            keygrab::set_input_focus(prev as std::os::raw::c_ulong);
        }

        let _ = win.emit("preedit", snap.clone());
        // Also re-emit on the next tick in case the webview was still waking up.
        let win2 = win.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            let _ = win2.emit("preedit", snap);
        });
    } else {
        let _ = win.emit("preedit", snap);
        let _ = win.hide();
        PREV_FOCUS.store(0, Ordering::SeqCst);
    }
}

/// How many raw pinyin letters have leaked into the focused app (because we
/// don't grab the keyboard). On commit we erase them with BackSpace before
/// injecting the Chinese text.
static LEAKED_CHARS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Set while we inject text synthetically (XTest). Our own XRecord capture
/// sees those keys; they must be ignored or they would corrupt the engine
/// state (e.g. the erase-BackSpaces would dismiss predictions).
static INJECTING: AtomicBool = AtomicBool::new(false);

/// Commit text to the focused app: first erase leaked pinyin letters, then
/// inject the committed text via the built-in XTest injector (UTF-8/CJK
/// capable, no external tools).
fn commit_text(_app: &AppHandle, _server: &ImeServer, text: &str) {
    // Restore keyboard focus to the app the user was typing into. Without
    // this the always-on-top candidate strip may hold focus and swallow the
    // injected text.
    let prev = PREV_FOCUS.load(Ordering::SeqCst);
    if prev != 0 {
        keygrab::set_input_focus(prev as std::os::raw::c_ulong);
        std::thread::sleep(std::time::Duration::from_millis(30));
    }

    let leaked = LEAKED_CHARS.swap(0, Ordering::SeqCst);
    if leaked == 0 && text.is_empty() {
        return;
    }
    INJECTING.store(true, Ordering::SeqCst);
    if leaked > 0 {
        keygrab::send_backspaces(leaked);
    }
    if !text.is_empty() {
        keygrab::inject_text(text);
    }
    // Ignore our own synthetic keys until the XRecord stream settles.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(200));
        INJECTING.store(false, Ordering::SeqCst);
    });
}

#[tauri::command]
fn feed_key(
    app: AppHandle,
    state: State<AppState>,
    key: String,
    ch: Option<char>,
) -> Result<Vec<String>, String> {
    let k = parse_key(&key, ch);
    let events = state.server.on_key(k);
    emit_preedit(&app, &state.server);

    // If the engine committed text, deliver it to the focused app.
    for ev in &events {
        if let EngineEvent::Commit(text) = ev {
            commit_text(&app, &state.server, text);
            // Clear XIM preedit after commit.
            if state.display.is_open() {
                let _ = unsafe { state.server.clear_preedit(state.display.as_ptr()) };
            }
        }
    }

    Ok(events
        .iter()
        .map(|e| match e {
            EngineEvent::PreeditChanged(_) => "preedit".to_string(),
            EngineEvent::Commit(t) => format!("commit:{t}"),
            EngineEvent::Candidates(_) => "candidates".to_string(),
            EngineEvent::Clear => "clear".to_string(),
            EngineEvent::Noop => "noop".to_string(),
        })
        .collect())
}

#[tauri::command]
fn commit_candidate(app: AppHandle, state: State<AppState>, idx: usize) -> Result<String, String> {
    let ev = state.server.commit_candidate(idx);
    emit_preedit(&app, &state.server);
    let committed = ev
        .iter()
        .find_map(|e| match e {
            EngineEvent::Commit(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default();
    if !committed.is_empty() {
        commit_text(&app, &state.server, &committed);
        if state.display.is_open() {
            let _ = unsafe { state.server.clear_preedit(state.display.as_ptr()) };
        }
    }
    Ok(committed)
}

#[tauri::command]
fn select_candidate(app: AppHandle, state: State<AppState>, _idx: usize) -> Result<(), String> {
    // Just move the selection highlight (arrow keys).
    let _ = state.server.on_key(ime_engine::Key::Right);
    emit_preedit(&app, &state.server);
    Ok(())
}

#[tauri::command]
fn reset_engine(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    state.server.reset();
    emit_preedit(&app, &state.server);
    if state.display.is_open() {
        let _ = unsafe { state.server.clear_preedit(state.display.as_ptr()) };
    }
    Ok(())
}

fn parse_key(name: &str, ch: Option<char>) -> ime_engine::Key {
    match name {
        "backspace" => ime_engine::Key::Backspace,
        "enter" => ime_engine::Key::Enter,
        "space" => ime_engine::Key::Space,
        "escape" => ime_engine::Key::Escape,
        "left" => ime_engine::Key::Left,
        "right" => ime_engine::Key::Right,
        "other" => ime_engine::Key::Other,
        _ if name.starts_with("digit") && name.len() == 6 => {
            ime_engine::Key::Digit(name.chars().nth(5).unwrap().to_digit(10).unwrap() as u8)
        }
        _ => {
            let c = name.chars().next().or(ch).unwrap_or(' ');
            if c.is_ascii_lowercase() {
                ime_engine::Key::Letter(c)
            } else if c.is_ascii_uppercase() {
                ime_engine::Key::Letter(c.to_ascii_lowercase())
            } else {
                ime_engine::Key::Other
            }
        }
    }
}

/// Whether to suppress consumed keys from reaching the focused app.
/// Off by default: `XGrabKeyboard` can hang the whole desktop if the ungrab
/// fails, so it's opt-in via `RUSTIME_SUPPRESS=1`.
fn suppress_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RUSTIME_SUPPRESS").ok().as_deref() == Some("1"))
}

/// X11 keycodes we track for the Ctrl+Space toggle.
const KC_CTRL_L: u32 = 37;
const KC_CTRL_R: u32 = 105;

static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);
static SHIFT_COMBINED: AtomicBool = AtomicBool::new(false);

const KC_SHIFT_L: u32 = 50;
const KC_SHIFT_R: u32 = 62;

/// Toggle Chinese/English input mode.
fn toggle_ime(app: &AppHandle) {
    let now = !IME_ACTIVE.load(Ordering::SeqCst);
    IME_ACTIVE.store(now, Ordering::SeqCst);
    let state = app.state::<AppState>();
    let _ = state.server.reset();
    LEAKED_CHARS.store(0, Ordering::SeqCst);
    emit_preedit(app, &state.server);
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.emit(
            "ime-mode",
            serde_json::json!({ "chinese": now }),
        );
    }
    eprintln!(
        "[rust-ime] IME {}",
        if now { "ON (中文)" } else { "OFF (English)" }
    );
}

/// Handle a grabbed key: decide whether to consume (IME mode) or pass through.
fn handle_grabbed_key(app: &AppHandle, gk: &GrabbedKey) {
    // fcitx5 owns input when our addon is active; ignore our own captures.
    if fcitx5_addon_active() {
        return;
    }
    // Ignore the synthetic keys we ourselves inject.
    if INJECTING.load(Ordering::SeqCst) {
        return;
    }
    // Track Ctrl state (press and release) for the Ctrl+Space toggle.
    if gk.keycode == KC_CTRL_L || gk.keycode == KC_CTRL_R {
        CTRL_DOWN.store(gk.is_press, Ordering::SeqCst);
        return; // never consume the modifier itself
    }
    // Track Shift: a bare tap toggles IME when configured; any other key
    // pressed while Shift is held marks it as a combination.
    if gk.keycode == KC_SHIFT_L || gk.keycode == KC_SHIFT_R {
        if gk.is_press {
            SHIFT_DOWN.store(true, Ordering::SeqCst);
            SHIFT_COMBINED.store(false, Ordering::SeqCst);
        } else {
            let tap = SHIFT_DOWN.swap(false, Ordering::SeqCst)
                && !SHIFT_COMBINED.load(Ordering::SeqCst);
            let want_shift = app
                .state::<AppState>()
                .settings
                .lock()
                .map(|s| s.toggle_key == "shift")
                .unwrap_or(false);
            if tap && want_shift {
                toggle_ime(app);
            }
        }
        return;
    }
    if SHIFT_DOWN.load(Ordering::SeqCst) {
        SHIFT_COMBINED.store(true, Ordering::SeqCst);
    }
    if !gk.is_press {
        return;
    }

    // Resolve the keycode → Key on THIS (dispatch) thread. The grabber thread
    // cannot do this without deadlocking.
    let key = keygrab::resolve_key(gk.keycode);
    let state = app.state::<AppState>();

    // Ctrl+Space: toggle Chinese/English mode (when configured).
    let want_ctrl_space = state
        .settings
        .lock()
        .map(|s| s.toggle_key == "ctrl_space")
        .unwrap_or(true);
    if key == ime_engine::Key::Space
        && CTRL_DOWN.load(Ordering::SeqCst)
        && want_ctrl_space
    {
        toggle_ime(app);
        return;
    }

    if !IME_ACTIVE.load(Ordering::SeqCst) {
        return;
    }

    let should_consume = matches!(
        key,
        ime_engine::Key::Letter(_)
            | ime_engine::Key::Digit(_)
            | ime_engine::Key::Backspace
            | ime_engine::Key::Enter
            | ime_engine::Key::Space
            | ime_engine::Key::Escape
            | ime_engine::Key::Left
            | ime_engine::Key::Right
    );
    if !should_consume {
        return;
    }

    if suppress_enabled() {
        keygrab::suppress_key_event();
    }

    // Without suppression, letters typed for the pinyin buffer also reach the
    // target app. Track how many so we can erase them on commit.
    if !suppress_enabled() {
        match key {
            ime_engine::Key::Letter(_) | ime_engine::Key::Digit(_) => {
                LEAKED_CHARS.fetch_add(1, Ordering::SeqCst);
            }
            ime_engine::Key::Backspace => {
                let _ = LEAKED_CHARS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    Some(n.saturating_sub(1))
                });
            }
            _ => {}
        }
    }

    let events = state.server.on_key(key);
    emit_preedit(app, &state.server);

    let mut committed = false;
    for ev in &events {
        if let EngineEvent::Commit(text) = ev {
            eprintln!("[rust-ime] commit: {text}");
            commit_text(app, &state.server, text);
            committed = true;
            if state.display.is_open() {
                let _ = unsafe { state.server.clear_preedit(state.display.as_ptr()) };
            }
        }
    }

    // Escape cancels the preedit: erase the leaked pinyin letters.
    if !committed && key == ime_engine::Key::Escape && !suppress_enabled() {
        let leaked = LEAKED_CHARS.swap(0, Ordering::SeqCst);
        if leaked > 0 {
            keygrab::send_backspaces(leaked);
        }
    }
}

/// Ensure only one instance runs. Returns a listener that must stay alive;
/// `None` means another instance already owns the socket.
fn single_instance() -> Option<std::os::unix::net::UnixListener> {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = format!("{dir}/rust-ime.sock");
    use std::os::unix::net::{UnixListener, UnixStream};
    // If a live instance answers, bail out. Otherwise clear the stale socket.
    if UnixStream::connect(&path).is_ok() {
        return None;
    }
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path).ok()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Headless maintenance entry points.
    if std::env::args().any(|a| a == "--install-addon") {
        match do_install_fcitx5_addon() {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if std::env::args().any(|a| a == "--uninstall-addon") {
        match do_uninstall_fcitx5_addon() {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let _instance = match single_instance() {
        Some(l) => l,
        None => {
            eprintln!("[rust-ime] another instance is already running");
            return;
        }
    };
    // Load persisted settings and the user dictionary.
    let settings = load_settings();
    install_desktop_entry();

    // Open X11 display for XIM commit + cursor tracking + key grab.
    let display = DisplayHandle::new();

    // Set up global key grab
    let (tx, rx) = mpsc::channel::<GrabbedKey>();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            server: ImeServer::with_user_dict(&user_dict_path()),
            display,
            settings: std::sync::Mutex::new(settings.clone()),
        })
        .setup(move |app| {
            let handle = app.handle().clone();

            // Apply persisted settings to the engine + autostart + UI.
            let state = app.state::<AppState>();
            apply_settings(&handle, &state.server, &settings);

            // System tray: click the logo to open settings, right-click for menu.
            if let Some(icon) = app
                .default_window_icon()
                .cloned()
                .or_else(|| tauri::image::Image::from_bytes(ICON_PNG).ok())
            {
                let settings_item =
                    MenuItem::with_id(app, "settings", "设置中心", true, None::<&str>)?;
                let quit_item =
                    MenuItem::with_id(app, "quit", "退出 rust-ime", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&settings_item, &quit_item])?;

                let tray = TrayIconBuilder::with_id("rust-ime-tray")
                    .icon(icon)
                    .tooltip("rust-ime 输入法")
                    .menu(&menu)
                    .show_menu_on_left_click(false)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "settings" => show_settings(app),
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .on_tray_icon_event(|tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            show_settings(tray.app_handle());
                        }
                    })
                    .build(app)?;
                // Keep the tray alive for the whole process lifetime.
                std::mem::forget(tray);
            }

            // `rust-ime-app --settings` opens the settings center directly.
            if std::env::args().any(|a| a == "--settings") {
                if let Some(w) = app.get_webview_window("settings") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }

            // If the fcitx5 addon is installed and fcitx5 is running, fcitx5
            // owns key handling — starting our own global grab would double
            // every keystroke.
            if fcitx5_addon_active() {
                eprintln!(
                    "[rust-ime] fcitx5 addon active — standalone key grab disabled"
                );
            } else {
                // Start the key grabber (opens its own X11 connection).
                match KeyGrabber::start(tx) {
                    Ok(grabber) => {
                        // Keep the grabber alive for the app's lifetime.
                        std::mem::forget(grabber);
                        eprintln!("[rust-ime] global key grabber started");
                    }
                    Err(e) => {
                        eprintln!("[rust-ime] keygrab failed: {e}");
                    }
                }
            }

            // Spawn reader thread: reads grabbed keys → feeds engine
            std::thread::Builder::new()
                .name("ime-key-dispatch".into())
                .spawn(move || {
                    for key in rx {
                        handle_grabbed_key(&handle, &key);
                    }
                })
                .expect("spawn key dispatch thread");

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            feed_key,
            commit_candidate,
            select_candidate,
            reset_engine,
            get_settings,
            save_settings,
            get_stats,
            get_system_status,
            set_fcitx5,
            install_fcitx5_addon,
            uninstall_fcitx5_addon,
            clear_user_dict,
            list_user_words,
            export_user_dict,
            open_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    run()
}
