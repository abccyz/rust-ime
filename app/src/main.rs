#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ime_engine::EngineEvent;
use ime_fcitx5::ImeServer;
use tauri::{AppHandle, Emitter, Manager, State};
use x11_dl::xlib;

/// Thread-safe wrapper for the X11 Display pointer.
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

const PAGE_SIZE: usize = 9;

fn snapshot(server: &ImeServer) -> PreeditView {
    let st = server.state();
    let total = st.candidates.len().div_ceil(PAGE_SIZE);
    let page = st.selected.div_ceil(PAGE_SIZE).min(total.saturating_sub(1));
    let start = page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(st.candidates.len());
    let page_candidates = &st.candidates[start..end];

    PreeditView {
        is_valid_pinyin: st.is_valid_pinyin,
        raw: st.raw.clone(),
        candidates: page_candidates
            .iter()
            .enumerate()
            .map(|(i, c)| CandidateView {
                text: c.text.clone(),
                pinyin: c.pinyin.clone(),
                selected: i + start == st.selected,
            })
            .collect(),
        page,
        total_pages: total,
    }
}

fn emit_preedit(app: &AppHandle, server: &ImeServer) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.emit("preedit", snapshot(server));
    }
}

/// Commit text to the focused app: try XIM first, fall back to XTest.
fn commit_text(app: &AppHandle, server: &ImeServer, text: &str) {
    let state = app.state::<AppState>();
    let display = state.display.as_ptr();

    if state.display.is_open() {
        // Try native XIM commit.
        let result = unsafe { server.commit_to_client(display, text) };
        match result {
            Ok(()) => return,
            Err(_) => {
                // No XIC connected — fall through to XTest.
            }
        }
    }
    // Fallback: XTest synthetic keys (ASCII only).
    if state.display.is_open() {
        let _ = unsafe { ime_fcitx5::commit::commit_text(display, text) };
    }
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
fn page_next(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    // Jump selection by PAGE_SIZE.
    for _ in 0..PAGE_SIZE {
        let _ = state.server.on_key(ime_engine::Key::Right);
    }
    emit_preedit(&app, &state.server);
    Ok(())
}

#[tauri::command]
fn page_prev(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    for _ in 0..PAGE_SIZE {
        let _ = state.server.on_key(ime_engine::Key::Left);
    }
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Open X11 display for XIM commit + cursor tracking.
    let display = DisplayHandle::new();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            server: ImeServer::new(),
            display,
        })
        .invoke_handler(tauri::generate_handler![
            feed_key,
            commit_candidate,
            select_candidate,
            page_next,
            page_prev,
            reset_engine
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main() {
    run()
}
