//! User dictionary persistence and learning.
//!
//! Mirrors `wave-ime`'s `oime_engine/src/pinyin/dict/usr_dict.cpp`: the user
//! types a phrase, selects a candidate, and the engine remembers that
//! (pinyin → word) mapping with a bumped frequency so next time it ranks
//! higher.
//!
//! The dictionary trie (see `crate::candidate::Dictionary`) is not iterable,
//! so persistence is driven by an explicit `Vec<(pinyin, word, freq)>`
//! **snapshot** the engine maintains in parallel. This is the same pragmatic
//! split the production path uses (a sidecar index next to the hash-sharded
//! `assets/dict/*` data).

use std::fs;
use std::path::Path;

use crate::candidate::Dictionary;

/// Load word entries from a tab-separated file into `dict`. Lines starting
/// with `#` and blank lines are ignored. A missing file is not an error — the
/// user dictionary may simply not exist yet.
pub fn load_words_file(path: &Path, dict: &mut Dictionary) -> std::io::Result<usize> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut n = 0;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let pinyin = parts.next().unwrap_or("").trim();
        let word = parts.next().unwrap_or("").trim();
        let freq = parts
            .next()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(1.0);
        if pinyin.is_empty() || word.is_empty() {
            continue;
        }
        dict.insert(pinyin, word, freq);
        n += 1;
    }
    Ok(n)
}

/// Bump the frequency of an existing `(pinyin, word)` entry in `dict`
/// (learning signal). If the entry is absent, add it with the bumped
/// frequency. Returns the new frequency.
pub fn reinforce(dict: &mut Dictionary, pinyin: &str, word: &str, bump: f64) -> f64 {
    let cur = dict
        .prefix_lookup(pinyin)
        .into_iter()
        .find(|(w, p, _)| w == word && p == pinyin)
        .map(|(_, _, f)| f)
        .unwrap_or(1.0);
    let newf = cur + bump;
    dict.insert(pinyin, word, newf);
    newf
}

/// Serialize a user-dictionary snapshot to `path` in the tab-separated
/// format. The file is truncated and rewritten.
pub fn save_snapshot(path: &Path, snapshot: &[(String, String, f64)]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut out = String::new();
    out.push_str("# rust-ime user dictionary\n# pinyin\tword\tlogfreq\n");
    for (p, w, f) in snapshot {
        out.push_str(p);
        out.push('\t');
        out.push_str(w);
        out.push('\t');
        out.push_str(&format!("{:.4}", f));
        out.push('\n');
    }
    fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_reinforce() {
        let mut d = Dictionary::new();
        d.insert("foo", "测试词", 1.0);
        let f = reinforce(&mut d, "foo", "测试词", 1.0);
        assert!(f >= 2.0);
    }

    #[test]
    fn load_missing_file_is_zero() {
        let mut d = Dictionary::new();
        let n = load_words_file(Path::new("/nonexistent/udict.txt"), &mut d).unwrap();
        assert_eq!(n, 0);
    }
}
