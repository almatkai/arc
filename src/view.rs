//! Typed wire subset consumed by Tezbar's GPUI extension renderer.
use crate::{
    browser::{Id, Location, Snapshot, Tab, WindowKey},
    data::{Download, History},
    search::{self, Suggestion},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
const PAGE: usize = 60;
const MAX_ROWS: usize = 420;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Item {
    Tab { tab: Tab, space_label: String },
    Space { id: Id, title: String, active: bool },
    History(History),
    Download(Download),
    Suggestion(Suggestion),
}

impl Item {
    pub fn title(&self) -> &str {
        match self {
            Self::Tab { tab, .. } => {
                if tab.title.is_empty() {
                    &tab.url
                } else {
                    &tab.title
                }
            }
            Self::Space { title, .. } => title,
            Self::History(history) => {
                if history.title.is_empty() {
                    &history.url
                } else {
                    &history.title
                }
            }
            Self::Download(download) => Path::new(&download.path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Download"),
            Self::Suggestion(suggestion) => &suggestion.text,
        }
    }

    pub fn url(&self) -> &str {
        match self {
            Self::Tab { tab, .. } => &tab.url,
            Self::History(history) => &history.url,
            Self::Download(download) => &download.url,
            Self::Suggestion(suggestion) => &suggestion.url,
            Self::Space { .. } => "",
        }
    }

    fn accessories(&self) -> Vec<Accessory> {
        match self {
            Self::Tab { space_label, .. } if !space_label.is_empty() => {
                vec![Accessory::Tag(space_label.clone())]
            }
            Self::Space { active: true, .. } => vec![Accessory::Tag("Active".into())],
            Self::History(history) => {
                let mut values = vec![Accessory::Text(history.time.clone())];
                if let Some(profile) = &history.profile {
                    values.push(Accessory::Tag(profile.clone()));
                }
                values
            }
            Self::Download(download) => vec![Accessory::Text(download.time.clone())],
            _ => Vec::new(),
        }
    }

    fn identity(&self, group: usize, row: usize) -> String {
        match self {
            Self::Tab { tab, .. } => format!(
                "tab:{}",
                serde_json::to_string(&tab.id).expect("Arc identity must serialize")
            ),
            Self::Space { id, .. } => format!(
                "space:{}",
                serde_json::to_string(id).expect("Arc identity must serialize")
            ),
            Self::Download(download) => format!("download:{}", download.id),
            _ => format!("row:{group}:{row}"),
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Group(pub String, pub Vec<Item>);

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum RowAction {
    Open,
    Little,
    Window,
    Incognito,
    SpaceChoice,
    BrowserChoice,
    Google,
    CopyUrl,
    CopyTitle,
    CopyMarkdown,
    QuickLink,
    Reload,
    Close,
    OpenFile,
    RevealFile,
    OpenDefaultBrowser,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Destination {
    Space {
        key: WindowKey,
        id: Id,
        title: String,
    },
    Browser {
        path: String,
        title: String,
    },
}

impl Destination {
    fn title(&self) -> &str {
        match self {
            Self::Space { title, .. } | Self::Browser { title, .. } => title,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Picker {
    pub title: String,
    pub url: String,
    pub choices: Vec<Destination>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Action {
    Refresh,
    Filter,
    Back,
    Destination(usize),
    Row {
        group: usize,
        row: usize,
        action: RowAction,
    },
}

#[derive(Default, Deserialize, Serialize)]
pub struct State {
    pub preferences: Value,
    pub query: String,
    pub filter: String,
    pub snapshot: Snapshot,
    pub snapshot_warning: Option<String>,
    pub installation_ok: bool,
    pub groups: Vec<Group>,
    pub warnings: Vec<String>,
    pub picker: Option<Picker>,
    pub revision: u64,
    pub actions: Vec<Action>,
    pub limit: usize,
}

impl State {
    pub fn action(&self, raw: &str) -> Result<Action, String> {
        let (revision, index) = raw.split_once(':').ok_or("Invalid Arc action ID")?;
        if revision.parse::<u64>().ok() != Some(self.revision) || self.revision == 0 {
            return Err("Stale Arc action. Refresh and try again.".into());
        }
        let index = index
            .parse::<usize>()
            .map_err(|_| "Invalid Arc action ID")?;
        self.actions
            .get(index)
            .cloned()
            .ok_or_else(|| "Unknown Arc action".into())
    }

    pub fn next_page(&mut self) {
        self.limit = (self.limit + PAGE).min(MAX_ROWS);
    }
    pub fn reset_page(&mut self) {
        self.limit = PAGE;
    }
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum Node {
    List {
        props: ListProps,
        children: Vec<Node>,
    },
    #[serde(rename = "List.Section")]
    Section {
        props: SectionProps,
        children: Vec<Node>,
    },
    #[serde(rename = "List.Item")]
    Item { props: ItemProps },
    #[serde(rename = "List.EmptyView")]
    Empty { props: EmptyProps },
    #[serde(rename = "List.Dropdown")]
    Dropdown {
        props: DropdownProps,
        children: Vec<Node>,
    },
    #[serde(rename = "List.Dropdown.Item")]
    Choice { props: ChoiceProps },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ListProps {
    filtering: bool,
    #[serde(rename = "__hasServerSearch")]
    server_search: bool,
    search_text: String,
    search_bar_placeholder: String,
    action_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_bar_accessory: Option<Box<Node>>,
}

#[derive(Serialize)]
struct SectionProps {
    title: String,
    subtitle: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemProps {
    id: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle: Option<Subtitle>,
    accessories: Vec<Accessory>,
    action_ids: Vec<String>,
}
#[derive(Serialize)]
struct Subtitle {
    value: String,
    tooltip: String,
}
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Accessory {
    Text(String),
    Tag(String),
}
#[derive(Serialize)]
struct EmptyProps {
    title: String,
    description: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DropdownProps {
    value: String,
    action_id: String,
    tooltip: &'static str,
}
#[derive(Serialize)]
struct ChoiceProps {
    title: &'static str,
    value: &'static str,
}
#[derive(Serialize)]
struct Shortcut {
    modifiers: &'static [&'static str],
    key: &'static str,
}
#[derive(Serialize)]
struct WireAction {
    id: String,
    title: &'static str,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    section: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shortcut: Option<Shortcut>,
}

struct Builder {
    revision: u64,
    actions: Vec<Action>,
    descriptors: Vec<WireAction>,
}

impl Builder {
    fn action(
        &mut self,
        action: Action,
        title: &'static str,
        section: Option<&'static str>,
        shortcut: Option<Shortcut>,
    ) -> String {
        let id = format!("{}:{}", self.revision, self.actions.len());
        self.actions.push(action);
        self.descriptors.push(WireAction {
            id: id.clone(),
            title,
            kind: "callback",
            section,
            shortcut,
        });
        id
    }

    fn row(&mut self, item: &Item, group: usize, row: usize, query: &str) -> Node {
        use RowAction::*;
        let mut action_ids = Vec::new();
        let mut add = |action, title, section, keys| {
            action_ids.push(self.action(
                Action::Row { group, row, action },
                title,
                Some(section),
                keys,
            ));
        };
        match item {
            Item::Space { .. } => {
                add(Open, "Open Space", "Open", None);
                add(
                    CopyTitle,
                    "Copy Space Title",
                    "Copy",
                    key(&["cmd", "shift"], "c"),
                );
            }
            Item::Download(row) => {
                add(OpenFile, "Open File", "File", None);
                add(RevealFile, "Show in Finder", "File", None);
                if !row.url.is_empty() {
                    add(
                        OpenDefaultBrowser,
                        "Open Download Tab in Browser",
                        "Open",
                        key(&["cmd", "shift"], "enter"),
                    );
                }
            }
            _ => {
                if matches!(item, Item::Tab { .. }) || !item.url().is_empty() {
                    add(Open, "Open in Arc", "Open", None);
                }
                if !item.url().is_empty() {
                    add(Little, "Open in Little Arc Window", "Open", None);
                    add(
                        Window,
                        "Open in New Window",
                        "Open",
                        key(&["cmd", "shift"], "enter"),
                    );
                    add(
                        Incognito,
                        "Open in Incognito Window",
                        "Open",
                        key(&["cmd", "opt", "shift"], "enter"),
                    );
                    add(
                        SpaceChoice,
                        "Open in Space",
                        "Open",
                        key(&["cmd", "opt"], "enter"),
                    );
                    add(
                        BrowserChoice,
                        "Open in Other Browser",
                        "Open",
                        key(&["cmd"], "o"),
                    );
                    add(CopyUrl, "Copy URL", "Copy", key(&["cmd"], "c"));
                }
                if !matches!(item, Item::Suggestion(_)) && !item.title().is_empty() {
                    add(CopyTitle, "Copy Title", "Copy", key(&["cmd", "shift"], "c"));
                    if !item.url().is_empty() {
                        add(
                            CopyMarkdown,
                            "Copy URL as Markdown",
                            "Copy",
                            key(&["cmd", "opt", "shift"], "c"),
                        );
                    }
                }
                if !query.is_empty() {
                    add(
                        Google,
                        "Search with Google",
                        "Open",
                        key(&["ctrl"], "enter"),
                    );
                }
                if matches!(item, Item::Tab { .. }) {
                    if !item.url().is_empty() {
                        add(QuickLink, "Create Quick Link", "Edit", key(&["cmd"], "s"));
                    }
                    add(Reload, "Reload Tab", "Edit", key(&["cmd", "shift"], "r"));
                    add(Close, "Close Tab", "Edit", key(&["cmd", "shift"], "w"));
                }
            }
        }
        let title = crate::bounded_text(item.title(), 460);
        let subtitle =
            (!item.url().is_empty() && !matches!(item, Item::Suggestion(_))).then(|| {
                let host = url::Url::parse(item.url())
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned));
                let value = host
                    .as_deref()
                    .unwrap_or(item.url())
                    .trim_start_matches("www.");
                Subtitle {
                    value: crate::bounded_text(value, 180),
                    tooltip: crate::bounded_text(item.url(), 430),
                }
            });
        Node::Item {
            props: ItemProps {
                id: item.identity(group, row),
                title,
                subtitle,
                accessories: item.accessories(),
                action_ids,
            },
        }
    }
}

fn key(modifiers: &'static [&'static str], key: &'static str) -> Option<Shortcut> {
    Some(Shortcut { modifiers, key })
}

pub fn tab_groups(state: &State, command: &str) -> Vec<Group> {
    let merged = command == "search";
    let snapshot = &state.snapshot;
    let records: Vec<_> = snapshot
        .tabs
        .iter()
        .filter_map(|tab| {
            if command == "search-space-tabs"
                && !snapshot
                    .active_space
                    .as_ref()
                    .is_some_and(|space| tab.spaces.contains(space))
            {
                return None;
            }
            let space_label = snapshot
                .spaces
                .iter()
                .enumerate()
                .filter(|(_, space)| tab.spaces.contains(&space.id))
                .map(|(index, _)| snapshot.space_label(index))
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            let score = if merged {
                [tab.title.as_str(), &tab.url, &space_label]
                    .iter()
                    .any(|value| search::matches(&state.query, value))
                    .then_some(0)
            } else {
                search::fuzzy_tab(&state.query, &tab.title, &tab.url, &space_label)
            }?;
            Some((score, tab, crate::bounded_text(&space_label, 300)))
        })
        .collect();
    let mut order = search::section_order(&state.preferences);
    order.push((Location::Unknown, "Other Tabs", "showUnpinnedTabs"));
    order
        .into_iter()
        .filter_map(|(location, title, visible)| {
            if merged && state.preferences[visible] == false {
                return None;
            }
            let filter_matches = match location {
                Location::TopApp => "topApp",
                Location::Pinned => "pinned",
                Location::Unpinned => "unpinned",
                Location::Unknown => "other",
            };
            if !merged && state.filter != "all" && state.filter != filter_matches {
                return None;
            }
            let mut entries: Vec<_> = records
                .iter()
                .filter(|(_, tab, _)| tab.location == location)
                .collect();
            entries.sort_by_key(|entry| entry.0);
            let items: Vec<_> = entries
                .into_iter()
                .map(|(_, tab, space_label)| Item::Tab {
                    tab: (*tab).clone(),
                    space_label: space_label.clone(),
                })
                .collect();
            (!items.is_empty()).then(|| Group(title.into(), items))
        })
        .collect()
}

pub fn render(command: &str, state: &mut State) -> Result<Value, String> {
    let revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
    let mut builder = Builder {
        revision,
        actions: Vec::new(),
        descriptors: Vec::new(),
    };
    let mut children = Vec::new();
    let mut more = false;
    let mut accessory = None;
    let mut global = Vec::new();
    let title;
    let placeholder;
    if let Some(picker) = &state.picker {
        title = picker.title.as_str();
        placeholder = "Filter destinations";
        global.push(builder.action(Action::Back, "Back to Arc Search", None, None));
        for (i, choice) in picker.choices.iter().enumerate() {
            let id = builder.action(Action::Destination(i), "Open Here", None, None);
            children.push(Node::Item {
                props: ItemProps {
                    id: format!("destination:{i}"),
                    title: choice.title().into(),
                    subtitle: None,
                    accessories: vec![],
                    action_ids: vec![id, global[0].clone()],
                },
            });
        }
        if children.is_empty() {
            children.push(Node::Empty {
                props: EmptyProps {
                    title: "No destinations found".into(),
                    description: "Return to Arc search to choose another action.".into(),
                },
            });
        }
    } else {
        title = match command {
            "search-history" => "Search History",
            "search-downloads" => "Search Downloads",
            "search-spaces" => "Search Spaces",
            "search-tabs" => "Search Tabs",
            "search-space-tabs" => "Search Space Tabs",
            _ => "Search Arc",
        };
        placeholder = title;
        global.push(builder.action(Action::Refresh, "Refresh", None, None));
        if matches!(command, "search-tabs" | "search-space-tabs") {
            let id = builder.action(Action::Filter, "Filter Tabs", None, None);
            let children = [
                ("All Tabs", "all"),
                ("Favorites", "topApp"),
                ("Pinned", "pinned"),
                ("Unpinned", "unpinned"),
            ]
            .into_iter()
            .map(|(title, value)| Node::Choice {
                props: ChoiceProps { title, value },
            })
            .collect();
            accessory = Some(Box::new(Node::Dropdown {
                props: DropdownProps {
                    value: state.filter.clone(),
                    action_id: id,
                    tooltip: "Filter tabs by tab type",
                },
                children,
            }));
        }
        let mut left = state.limit;
        for (group, Group(title, rows)) in state.groups.iter().enumerate() {
            if left == 0 || rows.is_empty() {
                continue;
            }
            let rows_visible = rows
                .iter()
                .take(left)
                .enumerate()
                .map(|(i, row)| builder.row(row, group, i, &state.query))
                .collect::<Vec<_>>();
            left -= rows_visible.len();
            let subtitle = format!(
                "{} {}",
                rows.len(),
                if rows.len() == 1 { "result" } else { "results" }
            );
            children.push(Node::Section {
                props: SectionProps {
                    title: title.clone(),
                    subtitle,
                },
                children: rows_visible,
            });
        }
        let total = state
            .groups
            .iter()
            .map(|group| group.1.len())
            .sum::<usize>();
        more = total > state.limit && state.limit < MAX_ROWS;
        for (index, warning) in state.warnings.iter().enumerate() {
            children.push(Node::Item {
                props: ItemProps {
                    id: format!("warning:{index}"),
                    title: warning.clone(),
                    subtitle: None,
                    accessories: vec![],
                    action_ids: global.clone(),
                },
            });
        }
        if total > MAX_ROWS && !more {
            children.push(Node::Item {
                props: ItemProps {
                    id: "limit".into(),
                    title: "Narrow search to see more matches".into(),
                    subtitle: None,
                    accessories: vec![],
                    action_ids: vec![],
                },
            });
        }
        if children.is_empty() {
            children.push(Node::Empty {
                props: EmptyProps {
                    title: "No Arc results".into(),
                    description: "Try another search or refresh the view.".into(),
                },
            });
        }
    }
    let server = state.picker.is_none();
    let root = Node::List {
        props: ListProps {
            filtering: !server,
            server_search: server,
            search_text: if server {
                state.query.clone()
            } else {
                String::new()
            },
            search_bar_placeholder: placeholder.to_owned(),
            action_ids: global,
            search_bar_accessory: accessory,
        },
        children,
    };
    let mut view = tezbar_extension_sdk::view(
        title,
        serde_json::to_value(root).map_err(|e| e.to_string())?,
        builder
            .descriptors
            .iter()
            .map(|item| serde_json::to_value(item).expect("Action serializes"))
            .collect(),
    );
    view["hasMore"] = Value::Bool(more);
    state.revision = revision;
    state.actions = builder.actions;
    Ok(view)
}
