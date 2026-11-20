use crate::browser::{self, Backend, Location};
use pinyin::ToPinyin;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tezbar_extension_sdk::{encode_component, ExtensionContext};
use tokio::process::Command;
use url::Url;

pub const TABS: [(Location, &str, &str, &str); 3] = [
    (
        Location::TopApp,
        "Favorites",
        "favoritesTabsOrder",
        "showFavorites",
    ),
    (
        Location::Pinned,
        "Pinned Tabs",
        "pinnedTabsOrder",
        "showPinnedTabs",
    ),
    (
        Location::Unpinned,
        "Unpinned Tabs",
        "unpinnedTabsOrder",
        "showUnpinnedTabs",
    ),
];

pub fn checked_query(value: &str) -> Result<String, String> {
    if value.len() > 4096 || value.contains('\0') {
        return Err("Arc search and arguments are limited to 4096 bytes".into());
    }
    Ok(value.trim().to_owned())
}

pub fn allowed_url(value: &str) -> Result<String, String> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return Err("Arc URL exceeds its bounds".into());
    }
    let url = Url::parse(value).map_err(|_| "Invalid Arc URL")?;
    if !matches!(
        url.scheme(),
        "https" | "http" | "ftp" | "arc" | "about" | "chrome" | "file"
    ) {
        return Err("Arc can only open supported browser URL schemes".into());
    }
    Ok(url.into())
}

fn network_or_internal_url(input: &str) -> Option<String> {
    if input.len() > 8192 || input.chars().any(char::is_whitespace) {
        return None;
    }
    if input.contains("://") || input.starts_with("about:") {
        return allowed_url(input).ok();
    }
    let url = Url::parse(&format!("https://{input}")).ok()?;
    let host = url.host_str()?;
    if !host.contains('.') && !host.starts_with('[') && host != "localhost" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    allowed_url(url.as_str()).ok()
}

pub fn strict_url(input: &str) -> Result<String, String> {
    network_or_internal_url(input)
        .ok_or_else(|| "Enter a valid URL or domain for the Arc tab".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Google,
    DuckDuckGo,
    Bing,
    Yahoo,
    Ecosia,
    Kagi,
    Unduck,
}

impl Engine {
    pub fn from_preferences(prefs: &Value) -> Self {
        match prefs["engine"].as_str() {
            Some("duckduckgo") => Self::DuckDuckGo,
            Some("bing") => Self::Bing,
            Some("yahoo") => Self::Yahoo,
            Some("ecosia") => Self::Ecosia,
            Some("kagi") => Self::Kagi,
            Some("unduck") => Self::Unduck,
            _ => Self::Google,
        }
    }

    pub fn search_url(self, query: &str) -> String {
        let base = match self {
            Self::Google => "https://www.google.com/search?q=",
            Self::DuckDuckGo => "https://www.duckduckgo.com?q=",
            Self::Bing => "https://www.bing.com/search?q=",
            Self::Yahoo => "https://search.yahoo.com/search?p=",
            Self::Ecosia => "https://www.ecosia.org/search?q=",
            Self::Kagi => "https://kagi.com/search?q=",
            Self::Unduck => "https://unduck.link?q=",
        };
        format!("{base}{}", encode_component(query))
    }

    fn suggest_url(self, query: &str) -> Option<String> {
        let base = match self {
            Self::Google => {
                "https://suggestqueries.google.com/complete/search?hl=en-us&output=chrome&q="
            }
            Self::Ecosia => "https://ac.ecosia.org?type=list&q=",
            Self::Kagi => "https://kagi.com/api/autosuggest?q=",
            _ => return None,
        };
        Some(format!("{base}{}", encode_component(query)))
    }

    pub fn classify(self, input: &str) -> String {
        network_or_internal_url(input).unwrap_or_else(|| self.search_url(input))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    pub text: String,
    pub url: String,
}

fn parse_suggestions(engine: Engine, body: &[u8]) -> Result<Vec<Suggestion>, String> {
    let decoded = String::from_utf8_lossy(body);
    let latin1: String;
    let text = if std::str::from_utf8(body).is_ok() {
        decoded.as_ref()
    } else {
        latin1 = body.iter().map(|byte| char::from(*byte)).collect();
        &latin1
    };
    let raw: Value =
        serde_json::from_str(text).map_err(|_| "Search engine returned invalid suggestions")?;
    let entries = raw[1]
        .as_array()
        .ok_or("Search engine returned invalid suggestions")?;
    Ok(entries
        .iter()
        .take(20)
        .enumerate()
        .filter_map(|(index, entry)| {
            let item = entry.as_str()?.trim();
            if item.is_empty() || item.len() > 750 {
                return None;
            }
            if engine == Engine::Google && raw[4]["google:suggesttype"][index] == "NAVIGATION" {
                let label = raw[2][index]
                    .as_str()
                    .filter(|text| !text.is_empty() && text.len() <= 750)
                    .unwrap_or(item);
                let url = network_or_internal_url(item)?;
                // Navigation suggestions can only create ordinary web links.
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return None;
                }
                Some(Suggestion {
                    text: label.into(),
                    url,
                })
            } else if engine != Engine::Google
                || raw[4]["google:suggesttype"][index]
                    .as_str()
                    .is_none_or(|kind| kind == "QUERY")
            {
                Some(Suggestion {
                    text: item.into(),
                    url: engine.search_url(item),
                })
            } else {
                None
            }
        })
        .collect())
}

pub async fn suggestions(
    ctx: &ExtensionContext,
    backend: &Backend,
    engine: Engine,
    query: &str,
    online: bool,
) -> (Vec<Suggestion>, Option<String>) {
    let mut rows = Vec::new();
    if query.is_empty() {
        return (rows, None);
    }
    if let Some(url) = network_or_internal_url(query) {
        rows.push(Suggestion {
            text: format!("Open URL {query}"),
            url,
        });
    }
    rows.push(Suggestion {
        text: query.into(),
        url: engine.search_url(query),
    });
    let Some(url) = online.then(|| engine.suggest_url(query)).flatten() else {
        return (rows, None);
    };
    let result = match backend {
        Backend::Native => {
            let mut command = Command::new("/usr/bin/curl");
            command.args([
                "--disable",
                "--proto",
                "=https",
                "--globoff",
                "--fail",
                "--silent",
                "--connect-timeout",
                "2",
                "--max-time",
                "4",
                "--max-filesize",
                "131072",
                "--config",
                "-",
            ]);
            let input = format!("url = \"{url}\"\n");
            browser::helper(
                ctx,
                command,
                input.as_bytes(),
                128 * 1024,
                Duration::from_secs(5),
            )
            .await
        }
        #[cfg(feature = "protocol-fixtures")]
        Backend::Fixture(path) => {
            crate::data::read_file(&path.join("suggestions.json"), 128 * 1024)
        }
    };
    match result.and_then(|body| parse_suggestions(engine, &body)) {
        Ok(items) => {
            for suggestion in items {
                if !rows.iter().any(|row| row.url == suggestion.url) {
                    rows.push(suggestion);
                }
            }
            (rows, None)
        }
        Err(_) => (rows, Some("Online suggestions are unavailable".into())),
    }
}

pub fn matches(query: &str, value: &str) -> bool {
    value.to_lowercase().contains(&query.to_lowercase())
}

pub fn fuzzy_tab(query: &str, title: &str, url: &str, spaces: &str) -> Option<usize> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let pinyin: String = title
        .to_pinyin()
        .filter_map(|letter| letter.map(|p| p.plain()))
        .collect();
    [
        title.to_lowercase(),
        url.to_lowercase(),
        spaces.to_lowercase(),
        pinyin,
    ]
    .iter()
    .filter_map(|text| fuzzy_field(&query, text))
    .min()
}

fn fuzzy_field(query: &str, text: &str) -> Option<usize> {
    if let Some(pos) = text.find(query) {
        return Some(pos.min(24));
    }
    let query: Vec<char> = query.chars().take(160).collect();
    if query.len() < 3 {
        return None;
    }
    let max_extra = (query.len() / 3).max(1);
    let mut matched = 0;
    let mut start = 0;
    for (index, ch) in text.chars().take(4096).enumerate() {
        if ch == query[0] && (matched == 0 || index - start > max_extra + matched) {
            start = index;
            matched = 1;
        } else if ch == query[matched] {
            matched += 1;
            if matched == query.len() {
                let extra = index + 1 - start - matched;
                if extra <= max_extra {
                    return Some(25 + extra);
                }
                matched = 0;
            }
        }
    }
    None
}

pub fn section_order(prefs: &Value) -> Vec<(Location, &'static str, &'static str)> {
    let mut order: Vec<_> = TABS
        .iter()
        .enumerate()
        .map(|(index, &(location, title, order, visible))| {
            let number = prefs[order]
                .as_str()
                .and_then(|value| value.parse::<u8>().ok())
                .filter(|value| (1..=3).contains(value))
                .unwrap_or(index as u8 + 1);
            (number, location, title, visible)
        })
        .collect();
    order.sort_by_key(|x| x.0);
    order
        .into_iter()
        .map(|(_, location, title, visible)| (location, title, visible))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_or_search_and_safe_protocols() {
        assert_eq!(
            strict_url("example.org/a?x=1").unwrap(),
            "https://example.org/a?x=1"
        );
        assert_eq!(strict_url("arc://newtab").unwrap(), "arc://newtab");
        assert!(strict_url("javascript:alert(1)").is_err());
        assert!(allowed_url("javascript:alert(1)").is_err());
        assert_eq!(
            Engine::DuckDuckGo.classify("two words & more"),
            "https://www.duckduckgo.com?q=two%20words%20%26%20more"
        );
    }
    #[test]
    fn supported_remote_payloads_and_pinyin() {
        let rows = parse_suggestions(Engine::Google, br#"["a",["example.org","one", "javascript://example.org"],["Home","","bad"],[],{"google:suggesttype":["NAVIGATION","QUERY","NAVIGATION"]}]"#).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].url, "https://example.org/");
        assert_eq!(rows[1].url, "https://www.google.com/search?q=one");
        let title = "\u{4e2d}\u{6587}";
        assert_eq!(fuzzy_tab("zhongwen", title, "", ""), Some(0));
        assert!(fuzzy_tab("wrok", "Work", "", "").is_none());
    }
}
