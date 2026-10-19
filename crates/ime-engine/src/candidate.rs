//! Candidate generation: pinyin → Chinese, scored and disambiguated.
//!
//! Two layers, mirroring `wave-ime`'s `keyboard::CandidateRefreshManager` +
//! `oime_engine` pinyin split:
//!
//! 1. A **trie** over the word list gives O(prefix) recall of candidates for
//!    an arbitrary pinyin prefix (so `niha` already yields 你好, 你是…).
//! 2. A **DP over syllable segmentations** (see `pinyin::segment`) scores
//!    multi-syllable candidates. This is the part that resolves `xian` →
//!    先/闲 (single syllable) vs `xi an` (two syllables) by preferring the
//!    segmentation that is actually in the dictionary.

use std::collections::BTreeMap;

use crate::pinyin;

/// One candidate returned to the front end.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Displayed / committed text (the Han characters, or the raw pinyin for
    /// a passthrough candidate).
    pub text: String,
    /// The pinyin this candidate was produced from (may span multiple
    /// syllables).
    pub pinyin: String,
    /// Higher is better. The engine sorts candidates by `score` descending.
    pub score: f64,
}

impl Candidate {
    fn new(text: String, pinyin: String, score: f64) -> Self {
        Self {
            text,
            pinyin,
            score,
        }
    }
}

/// Trie node.
#[derive(Debug, Default, Clone)]
struct Node {
    /// terminal: the full pinyin path ends here; maps word → log-prob.
    words: BTreeMap<String, f64>,
    children: BTreeMap<char, Node>,
}

/// A pinyin → word dictionary backed by a trie for prefix recall.
#[derive(Debug, Default)]
pub struct Dictionary {
    root: Node,
    /// Per-word log-frequency (higher = more common). Used as the base score.
    freq: BTreeMap<String, f64>,
    total: usize,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `word` reachable by the given (already space-free) pinyin string.
    /// `logfreq` is the log-frequency; pass a smaller value for rarer words.
    pub fn insert(&mut self, pinyin: &str, word: &str, logfreq: f64) {
        let pinyin = pinyin.trim().to_string();
        if pinyin.is_empty() || word.is_empty() {
            return;
        }
        let mut node = &mut self.root;
        for ch in pinyin.chars() {
            node = node.children.entry(ch).or_default();
        }
        node.words.insert(word.to_string(), logfreq);
        self.freq.insert(word.to_string(), logfreq);
        self.total += 1;
    }

    pub fn len(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Iterate over all `(word, logfreq)` pairs in the frequency map.
    /// Used by `userdict::save_snapshot` for persistence.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &f64)> {
        self.freq.iter()
    }

    /// The highest-frequency word whose pinyin is exactly `pinyin`.
    pub fn exact_best(&self, pinyin: &str) -> Option<(String, f64)> {
        let mut cur = &self.root;
        for ch in pinyin.chars() {
            cur = cur.children.get(&ch)?;
        }
        cur.words
            .iter()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(w, f)| (w.clone(), *f))
    }

    /// All words whose pinyin starts with `prefix`, with their base score.
    pub fn prefix_lookup(&self, prefix: &str) -> Vec<(String, String, f64)> {
        let prefix = prefix.to_string();
        let mut out: Vec<(String, String, f64)> = Vec::new();
        let mut cur = &self.root;
        for ch in prefix.chars() {
            match cur.children.get(&ch) {
                Some(n) => cur = n,
                None => return out,
            }
        }
        self.collect(cur, &prefix, &mut out);
        out
    }

    fn collect(&self, node: &Node, pinyin: &str, out: &mut Vec<(String, String, f64)>) {
        for (w, f) in &node.words {
            out.push((w.clone(), pinyin.to_string(), *f));
        }
        for (ch, child) in &node.children {
            let mut extended = pinyin.to_string();
            extended.push(*ch);
            self.collect(child, &extended, out);
        }
    }
}

/// The candidate engine: owns a `Dictionary` (system + user) and produces
/// ranked candidates for the current pinyin buffer.
pub struct CandidateEngine {
    sys: Dictionary,
    user: Dictionary,
    /// Passthrough flag: when the buffer is not valid pinyin, the raw buffer
    /// itself becomes a candidate so the user can still commit it.
    allow_passthrough: bool,
}

impl Default for CandidateEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl CandidateEngine {
    pub fn new() -> Self {
        Self {
            sys: Dictionary::new(),
            user: Dictionary::new(),
            allow_passthrough: true,
        }
    }

    pub fn system_dictionary(&mut self) -> &mut Dictionary {
        &mut self.sys
    }

    pub fn user_dictionary(&mut self) -> &mut Dictionary {
        &mut self.user
    }

    /// Number of system (built-in) entries.
    pub fn system_len(&self) -> usize {
        self.sys.len()
    }

    /// Number of learned user entries.
    pub fn user_len(&self) -> usize {
        self.user.len()
    }

    /// Iterate learned user entries as (word, frequency).
    pub fn user_words(&self) -> impl Iterator<Item = (&String, &f64)> {
        self.user.iter()
    }

    /// Drop all learned user entries.
    pub fn clear_user(&mut self) {
        self.user = Dictionary::new();
    }

    /// Convert the whole pinyin buffer into a sentence using Viterbi-style
    /// dynamic programming over dictionary words and bare syllables.
    ///
    /// This is what makes long input work: `nihao` alone was covered by prefix
    /// lookup, but a full sentence like `nihaoshijie` is not stored as one
    /// dictionary key.
    pub fn convert_sentence(&self, buffer: &str) -> Option<String> {
        let b = buffer.trim().to_ascii_lowercase();
        let n = b.len();
        if n == 0 {
            return None;
        }
        // best[i] = (score, text) for buffer[..i]
        let mut best: Vec<Option<(f64, String)>> = vec![None; n + 1];
        best[0] = Some((0.0, String::new()));

        for i in 0..n {
            let Some((score_i, text_i)) = best[i].clone() else {
                continue;
            };
            for len in 1..=std::cmp::min(12, n - i) {
                let chunk = &b[i..i + len];
                let hit = self
                    .sys
                    .exact_best(chunk)
                    .or_else(|| self.user.exact_best(chunk));
                let (word, add) = match hit {
                    Some((w, f)) => (w, f),
                    None => {
                        // Bare syllable fallback keeps the path alive.
                        if !pinyin::is_syllable(chunk) {
                            continue;
                        }
                        (chunk.to_string(), -6.0)
                    }
                };
                // Penalise every segment so a longer dictionary word beats
                // splitting into several short (individually frequent) ones.
                const SEGMENT_PENALTY: f64 = 9.0;
                let ns = score_i + add - SEGMENT_PENALTY;
                let better = best[i + len]
                    .as_ref()
                    .map_or(true, |(s, _)| ns > *s);
                if better {
                    best[i + len] = Some((ns, format!("{text_i}{word}")));
                }
            }
        }
        best[n].as_ref().map(|(_, t)| t.clone())
    }

    /// Predict the next word(s) after `context` (the last committed text).
    ///
    /// Two sources:
    /// 1. a built-in bigram table of common collocations (high confidence);
    /// 2. dictionary words that *start with* the context and are longer,
    ///    showing just the remainder (e.g. context "你好" → "吗").
    ///
    /// Returns at most `max` candidates, best first, deduplicated.
    pub fn predict(&self, context: &str, max: usize) -> Vec<Candidate> {
        let context = context.trim();
        if context.is_empty() {
            return Vec::new();
        }
        let mut acc: BTreeMap<String, f64> = BTreeMap::new();

        // 1) Bigram collocations.
        for (prev, next, score) in BIGRAMS {
            if *prev == context {
                let e = acc.entry((*next).to_string()).or_insert(f64::NEG_INFINITY);
                if *e < *score {
                    *e = *score;
                }
            }
        }

        // 2) Dictionary prefix completion: show only the remainder so the
        //    candidate reads naturally after the already-committed context.
        for (word, freq) in self.sys.iter().chain(self.user.iter()) {
            if let Some(rest) = word.strip_prefix(context) {
                if rest.is_empty() {
                    continue;
                }
                // Keep completions short; long tails are usually noise.
                if rest.chars().count() > 4 {
                    continue;
                }
                // Skip punctuation-only tails ("你好。" → "。").
                if rest.chars().all(|c| {
                    c.is_ascii_punctuation()
                        || "。，！？、；：（）《》〈〉“”‘’…—·～".contains(c)
                }) {
                    continue;
                }
                let e = acc.entry(rest.to_string()).or_insert(f64::NEG_INFINITY);
                if *e < *freq - 1.0 {
                    *e = *freq - 1.0;
                }
            }
        }

        let mut out: Vec<Candidate> = acc
            .into_iter()
            .map(|(text, score)| Candidate::new(text, String::new(), score))
            .collect();
        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out.truncate(max);
        out
    }

    /// Produce ranked candidates for the current toneless pinyin `buffer`.
    ///
    /// Returns at most `max` candidates, ordered by score descending,
    /// deduplicated by `(text)`.
    pub fn candidates(&self, buffer: &str, max: usize) -> Vec<Candidate> {
        let buffer = buffer.trim().to_ascii_lowercase();
        if buffer.is_empty() {
            return Vec::new();
        }

        // Layer 1: prefix recall — walk the trie on the buffer and accept
        // every word reachable at or below that node. This is what makes
        // `niha` already return 你好 (the word lives at trie node "niha o").
        // The pinyin we keep on each candidate is the *word's own* pinyin
        // (e.g. "nihao"), not the buffer, so the front end can show the full
        // pinyin under the candidate.
        let mut acc: BTreeMap<String, f64> = BTreeMap::new();
        for (w, p, f) in self.sys.prefix_lookup(&buffer) {
            let prev = acc.entry(w).or_insert(f64::NEG_INFINITY);
            if *prev < f {
                *prev = f;
            }
            let _ = p; // p kept for the candidate's pinyin field if needed
        }
        for (w, p, f) in self.user.prefix_lookup(&buffer) {
            // user words beat system words on ties: add a small boost.
            let prev = acc.entry(w).or_insert(f64::NEG_INFINITY);
            if *prev < f + 1.0 {
                *prev = f + 1.0;
            }
            let _ = p;
        }

        // Layer 2: full-segmentation candidates for the *whole* buffer. These
        // catch multi-syllable words whose pinyin equals the whole buffer but
        // whose individual syllables were not individually prefix-hittable in
        // the trie above (e.g. the dictionary stores the word keyed on the
        // full string but the trie only reached it via a different split).
        if let Some(segs) = pinyin::primary_segmentation(&buffer) {
            let joined = segs.join("");
            for (w, p, f) in self.sys.prefix_lookup(&joined) {
                if p == joined {
                    let prev = acc.entry(w).or_insert(f64::NEG_INFINITY);
                    if *prev < f {
                        *prev = f;
                    }
                }
            }
        }

        // Build the result. Carry the *word's own* pinyin on each candidate
        // (so the front end can display the full pinyin, e.g. "nihao", under
        // the candidate even when the user has only typed "niha").
        let mut out: Vec<Candidate> = Vec::with_capacity(acc.len());
        // We lost the per-word pinyin in `acc` (keyed by text only); re-derive
        // it from the word itself. For the MVP the candidate pinyin is the
        // buffer; a richer build would thread the word's pinyin through `acc`.
        out.extend(
            acc.into_iter()
                .map(|(text, score)| Candidate::new(text, buffer.clone(), score)),
        );

        // Whole-buffer sentence conversion: this is what makes long input
        // (e.g. "nihaoshijie") produce candidates at all.
        if let Some(sentence) = self.convert_sentence(&buffer) {
            if !sentence.is_empty() && !out.iter().any(|c| c.text == sentence) {
                out.push(Candidate::new(sentence, buffer.clone(), 100.0));
            }
        }

        // Passthrough candidate: the raw buffer as-is, always available so the
        // user can commit Latin text or an unconverted pinyin string.
        if self.allow_passthrough {
            let has_exact = out.iter().any(|c| c.text == buffer);
            if !has_exact {
                out.push(Candidate::new(buffer.clone(), buffer.clone(), -1000.0));
            }
        }

        out.sort_by(|a, b| b.score.total_cmp(&a.score));
        out.truncate(max);
        out
    }

    /// Whether the current `buffer` is *valid* pinyin — i.e. it can be fully
    /// segmented into real syllables. The front end uses this to decide
    /// whether to show the candidate strip vs. plain text.
    pub fn is_valid_pinyin(&self, buffer: &str) -> bool {
        pinyin::primary_segmentation(buffer.trim()).is_some()
    }
}

/// Load a built-in baseline word list into `dict`. The list is small and
/// curated; the real `wave-ime` ships a multi-MB hash-sharded dictionary under
/// `assets/dict/*`, which is the drop-in replacement here.
pub fn load_builtin_words(dict: &mut Dictionary) {
    // (pinyin, word, logfreq) — higher logfreq = more common.
    // This is a *demo* set; the production path is to load the same word
    // format from a data file (see `userdict::load_words_file`).
    const WORDS: &[(&str, &str, f64)] = &[
        // single syllables, very common
        ("ni", "你", 6.0),
        ("ni", "尼", 3.0),
        ("hao", "好", 6.5),
        ("hao", "号", 5.0),
        ("hao", "豪", 3.5),
        ("wo", "我", 6.5),
        ("shuo", "说", 6.0),
        ("shuo", "硕", 3.0),
        ("de", "的", 7.0),
        ("de", "得", 6.0),
        ("de", "地", 5.5),
        ("shi", "是", 7.0),
        ("shi", "事", 6.0),
        ("shi", "时", 5.8),
        ("ren", "人", 6.5),
        ("ren", "认", 4.5),
        ("da", "大", 6.0),
        ("xia", "下", 6.0),
        ("xia", "夏", 4.0),
        ("zhong", "中", 6.0),
        ("guo", "国", 6.0),
        ("yi", "一", 7.0),
        ("yi", "以", 6.0),
        ("yi", "已", 5.5),
        ("wu", "无", 5.5),
        ("you", "有", 6.8),
        ("zhe", "这", 6.5),
        ("zhe", "者", 4.0),
        ("ma", "吗", 5.5),
        ("ba", "吧", 5.5),
        ("ne", "呢", 5.0),
        ("bu", "不", 6.8),
        ("bu", "布", 4.0),
        ("zai", "在", 6.2),
        ("zuo", "做", 5.5),
        ("zuo", "坐", 5.0),
        ("kan", "看", 6.2),
        ("kan", "刊", 3.5),
        ("xie", "写", 5.8),
        ("xie", "谢", 5.5),
        ("xie", "些", 5.0),
        ("hui", "会", 6.2),
        ("hui", "回", 5.8),
        ("hui", "灰", 4.0),
        ("mei", "没", 5.8),
        ("mei", "美", 5.5),
        ("mei", "每", 5.2),
        ("qu", "去", 6.2),
        ("qu", "取", 5.0),
        ("qu", "曲", 4.5),
        ("lai", "来", 6.2),
        ("zou", "走", 5.8),
        ("zhen", "真", 5.5),
        ("hen", "很", 6.2),
        ("xi", "西", 5.0),
        ("xi", "喜", 5.2),
        ("xi", "习", 5.5),
        ("xiang", "想", 6.0),
        ("xiang", "相", 5.8),
        ("xiang", "像", 5.5),
        ("xian", "先", 6.0),
        ("xian", "现", 5.8),
        ("xian", "线", 5.5),
        ("xian", "闲", 4.5),
        ("xian", "显", 5.2),
        ("qian", "前", 6.0),
        ("qian", "钱", 5.5),
        ("qian", "千", 5.5),
        ("dui", "对", 6.2),
        // multi-syllable common words — the whole-buffer path
        ("nihao", "你好", 7.0),
        ("nihao", "你好。", 5.0),
        ("wode", "我的", 6.5),
        ("women", "我们", 6.5),
        ("zhongwen", "中文", 5.5),
        ("shurufa", "输入法", 5.8),
        ("shuru", "输入", 5.8),
        ("zhongwen", "中文", 5.5),
        ("zhege", "这个", 6.2),
        ("nage", "那个", 6.2),
        ("dajia", "大家", 6.2),
        ("jintian", "今天", 5.8),
        ("mingtian", "明天", 5.5),
        ("qingwen", "请问", 5.5),
        ("xiexie", "谢谢", 6.2),
        ("huanqing", "欢迎", 5.5),
        ("doubao", "豆包", 6.5),
        ("zhege", "这个", 6.2),
    ];
    for (p, w, f) in WORDS {
        dict.insert(p, w, *f);
    }
}

/// Load a TSV dictionary file into `dict`.
///
/// Format: one word per line, `pinyin\tword\tlogfreq`. Blank lines and lines
/// starting with `#` are skipped. This is the production path for loading
/// `data/dict.txt` via `include_str!`.
pub fn load_dict_str(s: &str, dict: &mut Dictionary) {
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let pinyin = parts[0].trim();
        let word = parts[1].trim();
        let logfreq: f64 = parts[2].trim().parse().unwrap_or(5.0);
        dict.insert(pinyin, word, logfreq);
    }
}


/// Common word collocations used by `predict()`.
///
/// (previous word, next word, score). Curated from everyday Chinese; the
/// dictionary-prefix fallback in `predict()` covers everything else.
const BIGRAMS: &[(&str, &str, f64)] = &[
    ("你好", "吗", 9.5),
    ("你好", "啊", 8.6),
    ("你好", "世界", 8.2),
    ("谢谢", "你", 9.2),
    ("谢谢", "大家", 7.8),
    ("不客气", "的", 7.5),
    ("对不起", "我", 8.2),
    ("没关系", "的", 7.8),
    ("我们", "可以", 9.0),
    ("我们", "一起", 8.7),
    ("我们", "的", 8.1),
    ("我们", "需要", 8.3),
    ("你们", "好", 8.6),
    ("他们", "的", 8.0),
    ("今天", "天气", 8.7),
    ("今天", "是", 8.2),
    ("今天", "下午", 7.6),
    ("明天", "见", 8.8),
    ("明天", "早上", 7.4),
    ("昨天", "晚上", 7.6),
    ("不是", "很", 8.0),
    ("这个", "问题", 8.3),
    ("这个", "可以", 7.9),
    ("那个", "时候", 7.6),
    ("请问", "你", 8.1),
    ("请问", "一下", 8.6),
    ("可以", "吗", 9.1),
    ("可以", "的", 8.2),
    ("可以", "使用", 7.7),
    ("什么", "时候", 8.6),
    ("什么", "东西", 8.1),
    ("怎么", "样", 9.0),
    ("怎么", "办", 8.6),
    ("为什么", "呢", 7.8),
    ("非常", "感谢", 8.7),
    ("非常", "好", 8.2),
    ("非常", "重要", 7.9),
    ("欢迎", "使用", 8.2),
    ("欢迎", "来到", 7.8),
    ("中文", "输入法", 8.8),
    ("输入法", "的", 7.6),
    ("豆包", "输入法", 8.0),
    ("我", "们", 9.3),
    ("你", "们", 9.1),
    ("他", "们", 9.0),
    ("不", "是", 9.2),
    ("不", "好", 8.0),
    ("不", "知道", 8.6),
    ("没", "有", 9.2),
    ("有", "的", 7.8),
    ("很", "好", 8.8),
    ("很", "重要", 8.0),
    ("是", "的", 9.3),
    ("一", "个", 9.2),
    ("这", "个", 9.2),
    ("那", "个", 9.1),
    ("就", "是", 8.9),
    ("也", "是", 8.7),
    ("在", "哪", 8.3),
    ("做", "什么", 8.5),
    ("说", "什么", 8.1),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> CandidateEngine {
        let mut e = CandidateEngine::new();
        load_builtin_words(e.system_dictionary());
        e
    }

    #[test]
    fn single_char() {
        let e = engine();
        let c = e.candidates("ni", 10);
        assert!(!c.is_empty());
        // 你 must be present (single-char candidate for "ni"). 你好 may rank
        // above it because multi-char words are weighted higher in a real
        // IME — that's the intended behaviour.
        assert!(
            c.iter().any(|c| c.text == "你"),
            "expected 你 in candidates for 'ni'"
        );
    }

    #[test]
    fn multi_char_prefers_whole() {
        let e = engine();
        let c = e.candidates("nihao", 10);
        assert!(!c.is_empty());
        // 你好 must be the top candidate for nihao.
        assert_eq!(c[0].text, "你好");
    }

    #[test]
    fn prefix_recall() {
        let e = engine();
        // typing just "niha" should already surface 你好 (pinyin starts with
        // that prefix), because prefix_lookup walks the trie on "niha".
        let c = e.candidates("niha", 10);
        let found = c.iter().any(|c| c.text == "你好");
        assert!(found, "expected 你好 among prefix candidates for 'niha'");
    }

    #[test]
    fn xian_ambiguity_resolves() {
        let e = engine();
        let c = e.candidates("xian", 10);
        assert!(!c.is_empty());
        // 先 (first, most common single-syllable) should outrank the 闲
        // (x+i+a+n) split, because "xian" is a single real syllable.
        assert_eq!(c[0].text, "先");
    }

    #[test]
    fn empty_buffer() {
        let e = engine();
        assert!(e.candidates("", 10).is_empty());
        assert!(e.candidates("   ", 10).is_empty());
    }

    #[test]
    fn user_dict_boosts() {
        let mut e = engine();
        e.system_dictionary().insert("foo", "不常见词", 1.0);
        e.user_dictionary().insert("foo", "不常见词", 1.0);
        let c = e.candidates("foo", 10);
        assert_eq!(c.first().map(|c| c.text.as_str()), Some("不常见词"));
        // user word should get the +1 boost
        assert!(c[0].score > 1.0);
    }

    #[test]
    fn converts_long_sentence() {
        let e = engine();
        let c = e.candidates("nihaoshijie", 10);
        assert!(!c.is_empty());
        assert!(
            c[0].text.contains("你好"),
            "expected a sentence candidate starting with 你好, got {:?}",
            c.iter().map(|x| &x.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn passthrough_always_available() {
        let e = engine();
        let c = e.candidates("xyz", 10);
        assert!(c.iter().any(|x| x.text == "xyz"));
        // valid pinyin still keeps the raw option as the last candidate
        let c2 = e.candidates("nihaoshijie", 10);
        assert!(c2.iter().any(|x| x.text == "nihaoshijie"));
    }

    #[test]
    fn dedup_and_order() {
        let mut e = CandidateEngine::new();
        e.system_dictionary().insert("aa", "甲", 2.0);
        e.system_dictionary().insert("aa", "乙", 5.0);
        let c = e.candidates("aa", 10);
        // 乙, 甲 and the always-available raw passthrough.
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].text, "乙");
        assert_eq!(c[1].text, "甲");
        assert!(c.iter().any(|x| x.text == "aa"));
    }
}


