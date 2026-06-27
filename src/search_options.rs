use regex::{Regex, RegexBuilder};

#[derive(Clone, Default)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

/// クエリと検索オプションをもとに構築されたマッチャー
pub struct Matcher {
    inner: MatcherInner,
}

enum MatcherInner {
    Plain {
        needle: String,
        case_sensitive: bool,
        whole_word: bool,
    },
    Regex(Regex),
}

impl Matcher {
    /// クエリと SearchOptions からマッチャーを構築する。
    /// 正規表現が不正な場合は None を返す。
    pub fn new(query: &str, opts: &SearchOptions) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        if opts.use_regex {
            let pattern = if opts.whole_word {
                format!(r"\b{query}\b")
            } else {
                query.to_owned()
            };
            let re = RegexBuilder::new(&pattern)
                .case_insensitive(!opts.case_sensitive)
                .build()
                .ok()?;
            Some(Self {
                inner: MatcherInner::Regex(re),
            })
        } else {
            let needle = if opts.case_sensitive {
                query.to_owned()
            } else {
                query.to_lowercase()
            };
            Some(Self {
                inner: MatcherInner::Plain {
                    needle,
                    case_sensitive: opts.case_sensitive,
                    whole_word: opts.whole_word,
                },
            })
        }
    }

    /// テキスト先頭からマッチ開始バイト位置を返す
    pub fn find(&self, text: &str) -> Option<usize> {
        self.find_from(text, 0)
    }

    /// `start` バイト位置以降でマッチの (開始, 終了) バイト位置を返す
    pub fn find_match_from(&self, text: &str, start: usize) -> Option<(usize, usize)> {
        let slice = text.get(start..)?;
        match &self.inner {
            MatcherInner::Plain {
                needle,
                case_sensitive,
                whole_word,
            } => {
                let haystack = if *case_sensitive {
                    slice.to_owned()
                } else {
                    slice.to_lowercase()
                };
                let mut offset = 0usize;
                while let Some(pos) = haystack[offset..].find(needle.as_str()) {
                    let abs_start = start + offset + pos;
                    let abs_end = abs_start + needle.len();
                    if !whole_word || is_word_boundary(text, abs_start, abs_end) {
                        return Some((abs_start, abs_end));
                    }
                    offset += pos + 1;
                }
                None
            }
            MatcherInner::Regex(re) => re.find(slice).map(|m| (start + m.start(), start + m.end())),
        }
    }

    /// `start` バイト位置以降でマッチ開始バイト位置を返す
    pub fn find_from(&self, text: &str, start: usize) -> Option<usize> {
        let slice = text.get(start..)?;
        match &self.inner {
            MatcherInner::Plain {
                needle,
                case_sensitive,
                whole_word,
            } => {
                let haystack = if *case_sensitive {
                    slice.to_owned()
                } else {
                    slice.to_lowercase()
                };
                let mut offset = 0usize;
                while let Some(pos) = haystack[offset..].find(needle.as_str()) {
                    let abs = start + offset + pos;
                    if !whole_word || is_word_boundary(text, abs, abs + needle.len()) {
                        return Some(abs);
                    }
                    offset += pos + 1;
                }
                None
            }
            MatcherInner::Regex(re) => re.find(slice).map(|m| start + m.start()),
        }
    }

    /// テキスト末尾から逆向きにマッチ開始バイト位置を返す（`end` バイトまで）
    pub fn rfind_in(&self, text: &str, end: usize) -> Option<usize> {
        let slice = text.get(..end)?;
        match &self.inner {
            MatcherInner::Plain {
                needle,
                case_sensitive,
                whole_word,
            } => {
                let haystack = if *case_sensitive {
                    slice.to_owned()
                } else {
                    slice.to_lowercase()
                };
                let mut search_end = haystack.len();
                loop {
                    let found = haystack[..search_end].rfind(needle.as_str())?;
                    if !whole_word || is_word_boundary(text, found, found + needle.len()) {
                        return Some(found);
                    }
                    if found == 0 {
                        return None;
                    }
                    search_end = found;
                }
            }
            MatcherInner::Regex(re) => re.find_iter(slice).map(|m| m.start()).last(),
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start]
        .chars()
        .next_back()
        .map(is_word_char)
        .unwrap_or(false);
    let after = text[end..]
        .chars()
        .next()
        .map(is_word_char)
        .unwrap_or(false);
    !before && !after
}
