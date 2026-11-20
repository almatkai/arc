mod browser;
mod data;
mod search;
mod tray;
mod view;

use browser::{Backend, Menu, Operation, TabAction};
use search::{allowed_url, checked_query, strict_url, Engine};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use tezbar_extension_sdk::{self as sdk, BoxFuture, CoreResult, Extension, ExtensionContext};
use view::{Action, Destination, Group, Item, Picker, RowAction, State};

const ID: &str = "com.tezbar.native.arc";
const COMMANDS: &[&str] = &[
    "search-history",
    "new-window",
    "new-blank-window",
    "new-little-arc",
    "search-spaces",
    "search-tabs",
    "search-space-tabs",
    "search-downloads",
    "new-incognito-window",
    "new-tab",
    "menu-bar",
    "search",
    "release-notes",
    "boost-gallery",
    "new-easel",
    "reset-space-tabs",
];
const VIEWS: &[&str] = &[
    "search-history",
    "search-spaces",
    "search-tabs",
    "search-space-tabs",
    "search-downloads",
    "search",
];
const BROWSERS: &[&str] = &[
    "com.apple.safari",
    "com.apple.safaritechnologypreview",
    "com.google.chrome",
    "com.google.chrome.canary",
    "org.mozilla.firefox",
    "org.mozilla.firefoxdeveloperedition",
    "com.brave.browser",
    "com.operasoftware.opera",
    "com.microsoft.edgemac",
    "com.microsoft.edgemac.beta",
    "com.vivaldi.vivaldi",
    "com.kagi.kagimacos",
    "app.zen-browser.zen",
    "company.thebrowser.dia",
    "ai.perplexity.comet",
];

struct ArcExtension {
    backend: Backend,
    tray: Arc<tray::Tray>,
    root: Mutex<Option<ExtensionContext>>,
}

pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn first_text<'a>(candidates: &[&'a str]) -> &'a str {
    candidates
        .iter()
        .copied()
        .find(|value| !value.trim().is_empty())
        .unwrap_or("")
}
pub fn bounded_text(input: &str, max: usize) -> String {
    let mut text = String::new();
    for ch in input.chars().filter(|ch| !ch.is_control()) {
        if text.len() + ch.len_utf8() > max {
            break;
        }
        text.push(ch);
    }
    text
}

impl ArcExtension {
    fn root(&self) -> Result<ExtensionContext, String> {
        self.root
            .lock()
            .map_err(|_| "Arc background ownership is unavailable")?
            .clone()
            .ok_or_else(|| "Arc process is uninitialized".into())
    }

    async fn one_shot(&self, ctx: &ExtensionContext, command: &str, payload: &Value) -> CoreResult {
        let args = &payload["argumentValues"];
        let prefs = &payload["preferences"];
        let input_url = checked_query(text(args, "url"))?;
        let fallback = checked_query(first_text(&[
            text(payload, "fallbackText"),
            text(payload, "searchText"),
        ]))?;
        let space = checked_query(first_text(&[text(args, "space"), text(prefs, "space")]))?;
        let engine = Engine::from_preferences(prefs);
        let operation = match command {
            "new-window" => {
                let (scope, space) = if space.is_empty() {
                    (None, None)
                } else {
                    let snapshot = self.backend.snapshot(ctx, true, false).await?;
                    (snapshot.key.clone(), snapshot.named_space(&space)?)
                };
                Operation::NewWindow {
                    scope,
                    space,
                    incognito: false,
                    url: None,
                }
            }
            "new-blank-window" => Operation::Menu {
                menu: Menu::BlankWindow,
            },
            "new-easel" => Operation::Menu { menu: Menu::Easel },
            "new-little-arc" => {
                let selected = if input_url.is_empty() {
                    ctx.host("host.selection.read", json!({"kind":"text"}))
                        .await
                        .unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                let selected = checked_query(selected.as_str().unwrap_or(""))?;
                let user_input = first_text(&[&input_url, &selected]);
                let default = prefs["url"].as_str().unwrap_or("https://google.com");
                let url = if user_input.is_empty() {
                    strict_url(first_text(&[&fallback, default]))?
                } else {
                    engine.classify(user_input)
                };
                Operation::Little {
                    url: allowed_url(&url)?,
                }
            }
            "new-incognito-window" => {
                let default = prefs["url"].as_str().unwrap_or("https://google.com");
                let url = if !input_url.is_empty() {
                    Some(allowed_url(&engine.classify(&input_url))?)
                } else if default.trim().is_empty() {
                    None
                } else {
                    Some(strict_url(default.trim())?)
                };
                Operation::NewWindow {
                    scope: None,
                    space: None,
                    incognito: true,
                    url,
                }
            }
            "new-tab" | "release-notes" | "boost-gallery" => {
                let input = match command {
                    "release-notes" => {
                        "https://resources.arc.net/hc/en-us/articles/20498285812375-Release-Notes"
                    }
                    "boost-gallery" => "https://arc.net/boosts",
                    _ => first_text(&[&input_url, &fallback, text(prefs, "url"), "arc://newtab"]),
                };
                let urls = input
                    .split(',')
                    .map(|url| strict_url(url.trim()))
                    .collect::<Result<Vec<_>, _>>()?;
                if urls.len() > 24 {
                    return Err("Open up to 24 Arc tabs at a time".into());
                }
                let snapshot = self.backend.snapshot(ctx, true, false).await?;
                let space = if command == "new-tab" {
                    snapshot.named_space(&space)?
                } else {
                    None
                };
                Operation::NewTabs {
                    key: snapshot.key.ok_or("No Arc window is ready")?,
                    space,
                    urls,
                }
            }
            "reset-space-tabs" => {
                let snapshot = self.backend.snapshot(ctx, false, false).await?;
                let key = snapshot.key.clone().ok_or("Arc has no current window")?;
                let space = snapshot
                    .active_space
                    .clone()
                    .ok_or("Arc has no active space")?;
                let index = snapshot
                    .spaces
                    .iter()
                    .position(|item| item.id == space)
                    .ok_or("Arc active space is unavailable")?;
                let name = bounded_text(&snapshot.space_label(index), 160);
                let answer = ctx.host("host.dialog.confirm", json!({"title":"Reset all tabs in this Arc Space?", "message":format!("Reset tabs in {name}.") ,"buttons":["Cancel","Reset Tabs"],"type":"warning"})).await?;
                if answer["response"] != 1 {
                    return Ok(sdk::no_view("Arc tab reset cancelled"));
                }
                Operation::Reset { key, space }
            }
            _ => return Err("Unknown Arc command".into()),
        };
        self.backend.run(ctx, operation).await?;
        hide(ctx).await;
        Ok(sdk::no_view("Arc command completed"))
    }

    async fn snapshot(
        &self,
        ctx: &ExtensionContext,
        command: &str,
        state: &mut State,
    ) -> Vec<String> {
        if !matches!(
            command,
            "search" | "search-tabs" | "search-space-tabs" | "search-spaces"
        ) {
            return Vec::new();
        }
        match self
            .backend
            .snapshot(ctx, true, command != "search-spaces")
            .await
        {
            Ok(snapshot) => {
                state.snapshot = snapshot;
                Vec::new()
            }
            Err(error) => {
                state.snapshot = Default::default();
                vec![error]
            }
        }
    }

    async fn rebuild(
        &self,
        ctx: &ExtensionContext,
        command: &str,
        state: &mut State,
        refresh: bool,
    ) -> Result<(), String> {
        if refresh {
            if let Err(error) = self.backend.check_installation(ctx).await {
                state.snapshot = Default::default();
                state.installation_ok = false;
                state.snapshot_warning = Some(error);
                state.groups.clear();
                state.warnings = state.snapshot_warning.iter().cloned().collect();
                return Ok(());
            }
            state.installation_ok = true;
            state.snapshot_warning = self.snapshot(ctx, command, state).await.into_iter().next();
        }
        state.warnings = state.snapshot_warning.iter().cloned().collect();
        if !state.installation_ok {
            return Ok(());
        }
        let mut groups = Vec::new();
        match command {
            "search-spaces" => {
                let spaces = state
                    .snapshot
                    .spaces
                    .iter()
                    .enumerate()
                    .filter_map(|(index, space)| {
                        let title = state.snapshot.space_label(index);
                        search::matches(&state.query, &title).then(|| Item::Space {
                            id: space.id.clone(),
                            title,
                            active: state.snapshot.active_space.as_ref() == Some(&space.id),
                        })
                    })
                    .collect();
                groups.push(Group("Spaces".into(), spaces));
            }
            "search-tabs" | "search-space-tabs" => {
                groups.extend(view::tab_groups(state, command));
            }
            "search-downloads" => {
                match data::query_downloads(ctx, self.backend.arc_dir()?, state.query.clone()).await
                {
                    Ok(rows) => groups.push(Group(
                        "Downloads".into(),
                        rows.into_iter().map(Item::Download).collect(),
                    )),
                    Err(error) => state.warnings.push(error),
                }
            }
            "search-history" | "search" => {
                let merged = command == "search";
                let history_enabled = !merged
                    || (state.preferences["showHistory"] != false
                        && state.snapshot.incognito != Some(true));
                let suggestions_enabled = merged && state.preferences["showSuggestions"] != false;
                let history = async {
                    if history_enabled {
                        data::query_history(
                            ctx,
                            self.backend.arc_dir()?,
                            state.query.clone(),
                            if merged { 25 } else { 200 },
                        )
                        .await
                    } else {
                        Ok(data::HistoryResults::default())
                    }
                };
                let suggestions = async {
                    if suggestions_enabled {
                        search::suggestions(
                            ctx,
                            &self.backend,
                            Engine::from_preferences(&state.preferences),
                            &state.query,
                            state.snapshot.incognito == Some(false),
                        )
                        .await
                    } else {
                        (Vec::new(), None)
                    }
                };
                let (history, (suggestions, warning)) = tokio::join!(history, suggestions);
                state.warnings.extend(warning);
                let history = match history {
                    Ok(history) => {
                        state.warnings.extend(history.warnings);
                        Group(
                            "History".into(),
                            history.rows.into_iter().map(Item::History).collect(),
                        )
                    }
                    Err(error) => {
                        state.warnings.push(error);
                        Group::default()
                    }
                };
                let tabs_first = state.preferences["sorting"] == "tabsHistorySuggestions";
                if tabs_first {
                    groups.extend(view::tab_groups(state, command));
                }
                groups.push(history);
                if merged && !tabs_first {
                    groups.extend(view::tab_groups(state, command));
                }
                if suggestions_enabled {
                    groups.push(Group(
                        "Suggestions".into(),
                        suggestions.into_iter().map(Item::Suggestion).collect(),
                    ));
                }
            }
            _ => return Err("Unknown Arc view".into()),
        }
        state.groups = groups;
        Ok(())
    }

    async fn row(
        &self,
        ctx: &ExtensionContext,
        command: &str,
        state: &mut State,
        group: usize,
        row: usize,
        action: RowAction,
    ) -> CoreResult {
        use RowAction::*;
        let item = state
            .groups
            .get(group)
            .and_then(|group| group.1.get(row))
            .ok_or("Unknown Arc result")?;
        let raw_url = item.url();
        let mut completed = "Opened in Arc";
        match action {
            Open => match item {
                Item::Tab { tab, .. } => {
                    let key = state
                        .snapshot
                        .key
                        .clone()
                        .ok_or("Missing Arc window identity")?;
                    self.backend
                        .run(
                            ctx,
                            Operation::Tab {
                                key,
                                tab: tab.id.clone(),
                                action: TabAction::Select,
                                expected_url: None,
                            },
                        )
                        .await?;
                }
                Item::Space { id, .. } => {
                    let key = state
                        .snapshot
                        .key
                        .clone()
                        .ok_or("Missing Arc window identity")?;
                    self.backend
                        .run(
                            ctx,
                            Operation::Space {
                                key,
                                space: id.clone(),
                            },
                        )
                        .await?;
                    completed = "Opened Arc Space";
                }
                _ => {
                    self.backend
                        .open_in_arc(ctx, &allowed_url(raw_url)?)
                        .await?
                }
            },
            Little => {
                self.backend
                    .run(
                        ctx,
                        Operation::Little {
                            url: allowed_url(raw_url)?,
                        },
                    )
                    .await?;
            }
            Window | Incognito => {
                self.backend
                    .run(
                        ctx,
                        Operation::NewWindow {
                            scope: None,
                            space: None,
                            incognito: matches!(action, Incognito),
                            url: Some(allowed_url(raw_url)?),
                        },
                    )
                    .await?;
            }
            SpaceChoice | BrowserChoice => {
                let url = allowed_url(raw_url)?;
                let (title, choices) = if matches!(action, SpaceChoice) {
                    let snapshot = self.backend.snapshot(ctx, true, false).await?;
                    let key = snapshot.key.clone().ok_or("No Arc window is ready")?;
                    let entries = snapshot
                        .spaces
                        .iter()
                        .enumerate()
                        .map(|(index, space)| Destination::Space {
                            key: key.clone(),
                            id: space.id.clone(),
                            title: bounded_text(&snapshot.space_label(index), 240),
                        })
                        .collect();
                    ("Open in Arc Space", entries)
                } else {
                    let apps = ctx.host("host.apps.list", json!({})).await?;
                    let entries = apps
                        .as_array()
                        .ok_or("Host application list is invalid")?
                        .iter()
                        .filter_map(|app| {
                            let bundle = text(app, "bundleId").to_ascii_lowercase();
                            let path = text(app, "path");
                            (BROWSERS.contains(&bundle.as_str()) && !path.is_empty()).then(|| {
                                Destination::Browser {
                                    path: path.into(),
                                    title: bounded_text(text(app, "name"), 240),
                                }
                            })
                        })
                        .take(64)
                        .collect();
                    ("Open in Other Browser", entries)
                };
                state.picker = Some(Picker {
                    title: title.into(),
                    url,
                    choices,
                });
                return view::render(command, state);
            }
            Google => {
                self.backend
                    .open_in_arc(ctx, &Engine::Google.search_url(&state.query))
                    .await?;
            }
            CopyUrl | CopyTitle | CopyMarkdown => {
                let value = match action {
                    CopyUrl => raw_url.to_owned(),
                    CopyTitle => item.title().into(),
                    _ => format!(
                        "[{}]({raw_url})",
                        item.title()
                            .replace('\\', "\\\\")
                            .replace('[', "\\[")
                            .replace(']', "\\]")
                    ),
                };
                sdk::copy(ctx, &value).await?;
                return view::render(command, state);
            }
            QuickLink => {
                let mut params = json!({ "link":allowed_url(raw_url)? });
                let name = bounded_text(item.title(), 64);
                if !name.trim().is_empty() {
                    params["name"] = json!(name);
                }
                ctx.host("host.quicklinks.create", params).await?;
                return view::render(command, state);
            }
            Reload | Close => {
                let Item::Tab { tab, .. } = item else {
                    return Err("Arc action requires a tab".into());
                };
                let key = state
                    .snapshot
                    .key
                    .clone()
                    .ok_or("Missing Arc window identity")?;
                self.backend
                    .run(
                        ctx,
                        Operation::Tab {
                            key,
                            tab: tab.id.clone(),
                            expected_url: Some(tab.url.clone()),
                            action: if matches!(action, Close) {
                                TabAction::Close
                            } else {
                                TabAction::Reload
                            },
                        },
                    )
                    .await?;
                self.rebuild(ctx, command, state, true).await?;
                return view::render(command, state);
            }
            OpenFile | RevealFile => {
                let Item::Download(download) = item else {
                    return Err("Arc action requires a download".into());
                };
                let path = Path::new(&download.path);
                if !path.is_absolute()
                    || download.path.chars().any(char::is_control)
                    || !self.download_exists(path)
                {
                    return Err("The download was moved or deleted".into());
                }
                if matches!(action, RevealFile) {
                    ctx.host("host.shell.reveal", json!({"path":download.path}))
                        .await?;
                    completed = "Revealed download";
                } else {
                    sdk::open(ctx, &download.path).await?;
                    completed = "Opened download";
                }
            }
            OpenDefaultBrowser => {
                sdk::open(ctx, &allowed_url(raw_url)?).await?;
                completed = "Opened download source";
            }
        }
        hide(ctx).await;
        Ok(sdk::no_view(completed))
    }

    fn download_exists(&self, path: &Path) -> bool {
        match &self.backend {
            Backend::Native => path.is_file(),
            #[cfg(feature = "protocol-fixtures")]
            Backend::Fixture(base) => path
                .canonicalize()
                .is_ok_and(|path| path.starts_with(base) && path.is_file()),
        }
    }

    async fn invoke(
        &self,
        ctx: &ExtensionContext,
        command: &str,
        state: &mut State,
        payload: &Value,
    ) -> CoreResult {
        match state.action(text(payload, "actionId"))? {
            Action::Refresh => self.rebuild(ctx, command, state, true).await?,
            Action::Back => {
                state.picker = None;
            }
            Action::Filter => {
                let filter = text(&payload["formValues"], "value");
                if !["all", "topApp", "pinned", "unpinned"].contains(&filter) {
                    return Err("Unknown Arc tab filter".into());
                }
                let saved = filter.to_owned();
                let store_command = command.to_owned();
                sdk::update_store(ctx, "tab-filters", move |store| {
                    store[store_command] = json!(saved);
                    Ok(())
                })
                .await?;
                state.filter = filter.into();
                state.reset_page();
                self.rebuild(ctx, command, state, false).await?;
            }
            Action::Destination(index) => {
                let picker = state
                    .picker
                    .as_ref()
                    .ok_or("Arc destination picker is closed")?;
                match picker.choices.get(index).ok_or("Unknown Arc destination")? {
                    Destination::Space { key, id, .. } => {
                        self.backend
                            .run(
                                ctx,
                                Operation::NewTabs {
                                    key: key.clone(),
                                    space: Some(id.clone()),
                                    urls: vec![picker.url.clone()],
                                },
                            )
                            .await?;
                    }
                    Destination::Browser { path, .. } => {
                        ctx.host(
                            "host.shell.openWithApp",
                            json!({"path":picker.url,"appPath":path}),
                        )
                        .await?;
                    }
                }
                hide(ctx).await;
                return Ok(sdk::no_view("Opened Arc link"));
            }
            Action::Row { group, row, action } => {
                return self.row(ctx, command, state, group, row, action).await
            }
        }
        view::render(command, state)
    }
}

async fn hide(ctx: &ExtensionContext) {
    // The browser operation already succeeded. A visibility failure must not cause a retry of that operation.
    let _ = ctx
        .host("host.window.visibility", json!({"visible":false}))
        .await;
}

impl Extension for ArcExtension {
    fn id(&self) -> &'static str {
        ID
    }
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
    fn commands(&self) -> &'static [&'static str] {
        COMMANDS
    }

    fn initialize<'a>(&'a self, ctx: &'a ExtensionContext) -> BoxFuture<'a, CoreResult> {
        Box::pin(async move {
            *self
                .root
                .lock()
                .map_err(|_| "Arc process is uninitialized")? = Some(ctx.clone());
            Ok(json!({"ready":true}))
        })
    }

    fn handle<'a>(
        &'a self,
        ctx: &'a ExtensionContext,
        command: &'a str,
        op: &'a str,
        payload: &'a Value,
        wire_state: &'a mut Value,
    ) -> BoxFuture<'a, CoreResult> {
        Box::pin(async move {
            if op == "extension:dispose-session" {
                *wire_state = json!({});
                return Ok(sdk::unchanged());
            }
            self.backend.supported(ctx)?;
            if command == "menu-bar" && op == "extension:run-command" {
                self.tray.enable(ctx, &self.root()?, &self.backend).await?;
                return Ok(sdk::no_view("Arc menu enabled"));
            }
            if !VIEWS.contains(&command) {
                if op != "extension:run-command" {
                    return Err("Unknown Arc operation".into());
                }
                return self.one_shot(ctx, command, payload).await;
            }
            let mut state: State = if op == "extension:run-command" {
                State::default()
            } else {
                serde_json::from_value(wire_state.clone())
                    .map_err(|_| "Arc view state is unavailable")?
            };
            let reply = match op {
                "extension:run-command" => {
                    state.preferences = payload["preferences"].clone();
                    state.query = checked_query(first_text(&[
                        text(payload, "fallbackText"),
                        text(payload, "searchText"),
                    ]))?;
                    if matches!(command, "search-tabs" | "search-space-tabs") {
                        let store = sdk::read_store(ctx, "tab-filters").await?;
                        let stored = text(&store, command);
                        state.filter = if ["all", "topApp", "pinned", "unpinned"].contains(&stored)
                        {
                            stored.into()
                        } else {
                            "unpinned".into()
                        };
                    }
                    state.reset_page();
                    self.rebuild(ctx, command, &mut state, true).await?;
                    view::render(command, &mut state)
                }
                "extension:invoke-action" => self.invoke(ctx, command, &mut state, payload).await,
                "extension:search-text-changed" => {
                    if state.picker.is_some() {
                        return Ok(sdk::unchanged());
                    }
                    state.query = checked_query(text(payload, "searchText"))?;
                    state.reset_page();
                    self.rebuild(ctx, command, &mut state, false).await?;
                    view::render(command, &mut state)
                }
                "extension:refresh-session" => {
                    if state.picker.is_some() {
                        return Ok(sdk::unchanged());
                    }
                    self.rebuild(ctx, command, &mut state, true).await?;
                    view::render(command, &mut state)
                }
                "extension:load-more" => {
                    state.next_page();
                    view::render(command, &mut state)
                }
                _ => Err("Unknown Arc operation".into()),
            }?;
            *wire_state = serde_json::to_value(state)
                .map_err(|_| "Arc view state could not be serialized")?;
            Ok(reply)
        })
    }

    fn background_activate<'a>(
        &'a self,
        ctx: &'a ExtensionContext,
        payload: &'a Value,
    ) -> BoxFuture<'a, CoreResult> {
        Box::pin(async move {
            self.backend.supported(ctx)?;
            if payload["key"] != tray::RESOURCE {
                return Err("Unknown Arc background resource".into());
            }
            self.tray.enable(ctx, &self.root()?, &self.backend).await
        })
    }

    fn background_event<'a>(
        &'a self,
        ctx: &'a ExtensionContext,
        payload: &'a Value,
    ) -> BoxFuture<'a, CoreResult> {
        Box::pin(async move {
            self.backend.supported(ctx)?;
            self.tray.event(ctx, &self.backend, payload).await
        })
    }
}

fn backend() -> Result<Option<Backend>, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [] => Ok(Some(Backend::Native)),
        [info] if info == "--protocol-info" => {
            println!(
                "{}",
                json!({"id":ID,"protocol":1,"version":env!("CARGO_PKG_VERSION"),"commands":COMMANDS,"fixtureSupport":cfg!(feature="protocol-fixtures")})
            );
            Ok(None)
        }
        #[cfg(feature = "protocol-fixtures")]
        [flag, root] if flag == "--fixtures" => {
            let root = std::path::PathBuf::from(root)
                .canonicalize()
                .map_err(|_| "Synthetic fixture directory is unavailable")?;
            if data::read_file(&root.join(".tezbar-arc-fixture"), 64)? != b"tezbar-arc-fixture-v1\n"
            {
                return Err("Missing synthetic fixture marker".into());
            }
            Ok(Some(Backend::Fixture(root)))
        }
        _ => Err("Unknown Arc extension option".into()),
    }
}

#[tokio::main(worker_threads = 2)]
async fn main() {
    let result = match backend() {
        Ok(None) => return,
        Ok(Some(backend)) => {
            sdk::serve(ArcExtension {
                backend,
                tray: Arc::default(),
                root: Mutex::default(),
            })
            .await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
