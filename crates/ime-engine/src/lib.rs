//! `ime-engine` — a pure-Rust pinyin input-method engine.
//!
//! This is the Linux rebuild of the core of ByteDance's 豆包输入法
//! (reverse-engineered from `libkeyboard.so`, the `keyboard::` C++ engine of the
//! internal `wave-ime` framework). Rather than FFI-ing the original C++ binary,
//! the engine is reimplemented in Rust with the same observable contract that
//! the Android `ImeService` drove through `KeyboardJni`:
//!
//! ```text
//!   key events  →  engine  →  (preedit, candidates, commit)
//!   Engine::feed_key('n')      → update preedit "ni"
//!   Engine::commit_candidate(0)→ commit "你", clear preedit
//!   Engine::feed_key(Backspace)→ clear preedit
//!   Engine::finish(false)      → commit the first candidate
//! ```
//!
//! Scope of this MVP:
//! * pinyin 26-key input with toneless syllables,
//! * local candidate generation from a built-in word list + user dictionary,
//! * candidate selection by number / mouse index,
//! * commit of text to the front end.
//!
//! Out of scope (post-MVP, mirrors the wave-ime cloud/LLM layers):
//! cloud convert, word association, 豆包 LLM completion, speech (ASR),
//! handwriting, translation.
//!
//! The engine is front-end agnostic: it knows nothing about fcitx5, X11 or
//! WebKit. The fcitx5 integration (`ime-fcitx5`) and the Tauri front end both
//! call into this crate.

pub mod candidate;
pub mod engine;
pub mod ffi;
pub mod pinyin;
pub mod userdict;

pub use engine::{Engine, EngineEvent, Key, PreeditState};
