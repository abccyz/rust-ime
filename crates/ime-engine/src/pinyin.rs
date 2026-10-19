//! Pinyin syllables and string segmentation.
//!
//! The engine works on *toneless* pinyin: the user types `nihao` and we
//! segment it into syllables `ni` + `hao`. Segmenting a bare pinyin string is
//! ambiguous (e.g. `xian` → `xian` vs `xi an`), so we resolve ambiguity by
//! preferring segmentations whose *longest* syllables cover the string.

use std::collections::HashSet;
use std::sync::OnceLock;

static SYLLABLE_SET: OnceLock<HashSet<String>> = OnceLock::new();

fn syllable_set() -> &'static HashSet<String> {
    SYLLABLE_SET.get_or_init(|| SYLLABLES.iter().map(|s| s.to_string()).collect())
}

/// The complete set of ~410 toneless pinyin syllables.
///
/// Covers all initials (b/p/m/f/d/t/n/l/g/k/h/j/q/x/z/c/s/zh/ch/sh/r)
/// plus zero-initial syllables (a/o/e/i/u/w/y families).
pub const SYLLABLES: &[&str] = &[
    // zero initials
    "a", "o", "e", "ai", "ei", "ao", "ou", "an", "en", "ang", "eng", "ong", "er", "yi", "wu", "yu",
    "ya", "ye", "yao", "you", "yan", "yin", "yang", "ying", "yong", "yuan", "yue", "yun", "yo",
    "wa", "wo", "wai", "wei", "wan", "wen", "wang", // b
    "ba", "bo", "bai", "bei", "bao", "bi", "bian", "bie", "biao", "bin", "bing", "beng", "ban",
    "ben", "bang", "bu", // p
    "pa", "po", "pai", "pei", "pao", "pi", "pian", "pie", "piao", "pin", "ping", "peng", "pan",
    "pen", "pang", "pu", // m
    "ma", "mo", "me", "mai", "mei", "mao", "mi", "mian", "mie", "miao", "min", "ming", "meng",
    "man", "men", "mang", "mu", // f
    "fa", "fo", "fei", "fou", "fu", "fan", "fen", "fang", "feng", // d
    "da", "de", "dai", "dao", "di", "dian", "die", "diao", "diu", "dong", "dou", "du", "dui",
    "dun", "duo", "dan", "dang", "deng", // t
    "ta", "te", "tai", "tao", "ti", "tian", "tie", "tiao", "tong", "tou", "tu", "tui", "tun",
    "tuo", "tan", "tang", "teng", // n
    "na", "ne", "nei", "nai", "nao", "niu", "ni", "nian", "nie", "niang", "nin", "ning", "nong",
    "nou", "nu", "nun", "nuo", "nan", "nang", "neng", // l
    "la", "le", "lei", "lai", "lao", "liu", "li", "lian", "lie", "liang", "lin", "ling", "long",
    "lou", "lu", "lun", "luo", "lan", "lang", "leng", // g
    "ga", "ge", "gei", "gai", "gao", "gu", "gua", "guai", "guang", "guan", "gun", "guo", "gou",
    "gen", "gan", "gang", "geng", // k
    "ka", "ke", "kai", "kao", "ku", "kua", "kuai", "kuang", "kuan", "kun", "kuo", "kou", "ken",
    "kan", "kang", "keng", // h
    "ha", "he", "hei", "hai", "hao", "hu", "hua", "huai", "huang", "huan", "hun", "huo", "hou",
    "hen", "han", "hang", "heng", // j
    "ji", "jia", "jiao", "jiu", "ju", "juan", "jun", "jue", "jie", "jin", "jing", "jiong", "jiang",
    "jian", // q
    "qi", "qia", "qiao", "qiu", "qu", "quan", "qun", "que", "qie", "qin", "qing", "qiong", "qiang",
    "qian", // x
    "xi", "xia", "xiao", "xiu", "xu", "xuan", "xun", "xue", "xie", "xin", "xing", "xiong", "xiang",
    "xian", // z
    "za", "ze", "zei", "zai", "zao", "zi", "zuo", "zou", "zuan", "zun", "zui", "zen", "zan",
    "zang", "zeng", // c
    "ca", "ce", "cai", "cao", "ci", "cuo", "cou", "cuan", "cun", "cui", "cen", "can", "cang",
    "ceng", // s
    "sa", "se", "sai", "sao", "si", "suo", "sou", "suan", "sun", "sui", "sen", "san", "sang",
    "seng", // zh
    "zha", "zhe", "zhei", "zhai", "zhao", "zhi", "zhuo", "zhou", "zhuan", "zhun", "zhu", "zhuang",
    "zhuai", "zhen", "zhan", "zhang", "zheng", "zhong", // ch
    "cha", "che", "chai", "chao", "chi", "chu", "chuo", "chou", "chuan", "chun", "chuai", "chua",
    "chen", "chan", "chang", "cheng", "chong", "chui", // sh
    "sha", "she", "shai", "shao", "shi", "shu", "shuo", "shou", "shuan", "shun", "shua", "shuai",
    "shen", "shan", "shang", "sheng", "shui", // r
    "re", "ri", "ruo", "rou", "ruan", "run", "ru", "ren", "rang", "rui", "rong",
];

/// Whether `s` is a valid toneless pinyin syllable.
pub fn is_syllable(s: &str) -> bool {
    syllable_set().contains(s)
}

/// Fold dialect-sensitive spellings onto one canonical form so both the
/// dictionary and the query match under the same rules:
///
/// * zh/z, ch/c, sh/s (retroflex vs. flat)
/// * ang/an, eng/en, ing/in (front vs. back nasal)
/// * a word-initial n/l
pub fn fuzzy_key(s: &str) -> String {
    let mut out = s.replace("zh", "z").replace("ch", "c").replace("sh", "s");
    out = out
        .replace("ang", "an")
        .replace("eng", "en")
        .replace("ing", "in");
    if out.starts_with('n') {
        out.replace_range(0..1, "l");
    }
    out
}

/// All valid ways to segment `input` into pinyin syllables. Each result is a
/// `Vec<String>` of syllables whose concatenation equals `input`. Longer-
/// syllable segmentations are listed first.
pub fn segment(input: &str) -> Vec<Vec<String>> {
    let input = input.to_ascii_lowercase();
    let n = input.len();
    if n == 0 {
        return vec![];
    }
    let set = syllable_set();
    let mut dp: Vec<Vec<Vec<String>>> = vec![Vec::new(); n + 1];
    dp[n].push(Vec::new());
    for i in (0..n).rev() {
        let mut out: Vec<Vec<String>> = Vec::new();
        for len in (1..=std::cmp::min(n - i, 6)).rev() {
            let cand = &input[i..i + len];
            if set.contains(cand) {
                let rest = &dp[i + len];
                for seg in rest {
                    let mut newseg = vec![cand.to_string()];
                    newseg.extend(seg.iter().cloned());
                    out.push(newseg);
                }
            }
        }
        dp[i] = out;
    }
    dp[0].clone()
}

/// The primary segmentation: the first (longest-syllable-biased) one from
/// `segment`, or `None` if `input` cannot be segmented.
pub fn primary_segmentation(input: &str) -> Option<Vec<String>> {
    segment(input).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_syllables() {
        assert!(is_syllable("ni"));
        assert!(is_syllable("hao"));
        assert!(is_syllable("xian"));
        assert!(!is_syllable("abc"));
        assert!(!is_syllable(""));
    }

    #[test]
    fn segment_nihao() {
        let segs = segment("nihao");
        assert!(!segs.is_empty());
        assert_eq!(segs[0], vec!["ni", "hao"]);
    }

    #[test]
    fn segment_xian_prefers_whole() {
        let segs = segment("xian");
        assert!(!segs.is_empty());
        assert_eq!(segs[0], vec!["xian"]);
    }

    #[test]
    fn segment_unsegmentable() {
        assert!(segment("xyz").is_empty());
    }

    #[test]
    fn primary_matches_first() {
        assert_eq!(
            primary_segmentation("nihao"),
            Some(vec!["ni".into(), "hao".into()])
        );
    }

    #[test]
    fn er_syllable() {
        assert!(is_syllable("er"));
        assert_eq!(segment("er"), vec![vec!["er".to_string()]]);
    }

    #[test]
    fn w_family() {
        for s in ["wa", "wo", "wai", "wei", "wan", "wen", "wang", "wu"] {
            assert!(is_syllable(s), "missing w-syllable: {s}");
        }
        assert_eq!(primary_segmentation("wang"), Some(vec!["wang".to_string()]));
        assert_eq!(
            primary_segmentation("wenhua"),
            Some(vec!["wen".to_string(), "hua".to_string()])
        );
    }

    #[test]
    fn y_family() {
        for s in [
            "ya", "ye", "yao", "you", "yan", "yin", "yang", "ying", "yong", "yuan", "yue", "yun",
            "yi", "yo",
        ] {
            assert!(is_syllable(s), "missing y-syllable: {s}");
        }
        assert_eq!(primary_segmentation("yuan"), Some(vec!["yuan".to_string()]));
        assert_eq!(primary_segmentation("yang"), Some(vec!["yang".to_string()]));
    }

    #[test]
    fn no_duplicates_in_syllables() {
        let mut seen = HashSet::new();
        for s in SYLLABLES {
            assert!(seen.insert(*s), "duplicate syllable: {s}");
        }
    }

    #[test]
    fn common_phrases() {
        assert_eq!(
            primary_segmentation("zhongwen"),
            Some(vec!["zhong".to_string(), "wen".to_string()])
        );
        assert_eq!(
            primary_segmentation("zenmeban"),
            Some(vec!["zen".to_string(), "me".to_string(), "ban".to_string()])
        );
        assert_eq!(
            primary_segmentation("xihuan"),
            Some(vec!["xi".to_string(), "huan".to_string()])
        );
    }
}
