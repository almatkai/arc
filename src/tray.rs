use crate::{
    bounded_text,
    browser::{Backend, Location, Operation, TabAction},
    text,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tezbar_extension_sdk::{CoreResult, ExtensionContext};
use tokio::sync::{Mutex, Notify};

pub const RESOURCE: &str = "arc-sidebar";

#[derive(Clone)]
enum Callback {
    Arc(Operation),
    Refresh,
    Settings,
    Disable,
}

#[derive(Default)]
struct State {
    active: bool,
    worker_started: bool,
    callbacks: HashMap<String, Callback>,
}

#[derive(Default)]
pub struct Tray {
    state: Mutex<State>,
    change: Notify,
    revision: AtomicU64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    id: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_name: Option<&'static str>,
}

impl Tray {
    async fn refresh(
        &self,
        ctx: &ExtensionContext,
        backend: &Backend,
        state: &mut State,
    ) -> CoreResult {
        let mut items = Vec::new();
        let mut callbacks = HashMap::new();
        let revision = self.revision.fetch_add(1, Ordering::Relaxed);
        let mut add = |title: String, kind: Option<&'static str>, command_name, callback| {
            let id = format!("r{revision}-{}", items.len());
            if let Some(callback) = callback {
                callbacks.insert(id.clone(), callback);
            }
            items.push(Item {
                id,
                title,
                kind,
                command_name,
            });
        };
        match backend.snapshot(ctx, false, true).await {
            Ok(snapshot) => {
                if let Some(key) = &snapshot.key {
                    if !snapshot.spaces.is_empty() {
                        add("Spaces".into(), Some("section"), None, None);
                        for (index, space) in snapshot.spaces.iter().take(25).enumerate() {
                            add(
                                bounded_text(&snapshot.space_label(index), 95),
                                None,
                                None,
                                Some(Callback::Arc(Operation::Space {
                                    key: key.clone(),
                                    space: space.id.clone(),
                                })),
                            );
                        }
                    }
                    let mut favorites = snapshot
                        .tabs
                        .iter()
                        .filter(|tab| tab.location == Location::TopApp)
                        .take(25)
                        .peekable();
                    if favorites.peek().is_some() {
                        add("Favorite Tabs".into(), Some("section"), None, None);
                    }
                    for tab in favorites {
                        let title = if tab.title.is_empty() {
                            &tab.url
                        } else {
                            &tab.title
                        };
                        add(
                            bounded_text(title, 95),
                            None,
                            None,
                            Some(Callback::Arc(Operation::Tab {
                                key: key.clone(),
                                tab: tab.id.clone(),
                                action: TabAction::Select,
                                expected_url: None,
                            })),
                        );
                    }
                } else {
                    add("Arc has no open window".into(), Some("section"), None, None);
                }
            }
            Err(_) => add(
                "Arc sidebar is unavailable".into(),
                Some("section"),
                None,
                None,
            ),
        }
        add("View All Spaces".into(), None, Some("search-spaces"), None);
        add("View All Tabs".into(), None, Some("search-tabs"), None);
        add(
            "Configure Command".into(),
            None,
            None,
            Some(Callback::Settings),
        );
        add(
            "Refresh Arc Menu".into(),
            None,
            None,
            Some(Callback::Refresh),
        );
        add("Hide Arc Menu".into(), None, None, Some(Callback::Disable));
        ctx.host("host.tray.set", json!({"resourceId":RESOURCE,"title":"Arc","tooltip":"Access Arc Spaces and Favorites","icon":"resources/icon.png","items":items})).await?;
        state.callbacks = callbacks;
        Ok(json!({"ready":true}))
    }

    pub async fn enable(
        self: &Arc<Self>,
        ctx: &ExtensionContext,
        root: &ExtensionContext,
        backend: &Backend,
    ) -> CoreResult {
        let mut state = self.state.lock().await;
        if state.active {
            return self.refresh(ctx, backend, &mut state).await;
        }
        if !state.worker_started {
            let owner = Arc::clone(self);
            let backend = backend.clone();
            let worker = root.clone();
            root.spawn(async move {
                loop {
                    let is_active = owner.state.lock().await.active;
                    if !is_active {
                        owner.change.notified().await;
                        continue;
                    }
                    tokio::select! {
                        _ = owner.change.notified() => {},
                        _ = tokio::time::sleep(Duration::from_secs(600)) => {
                            let mut state = owner.state.lock().await;
                            if state.active { let _ = owner.refresh(&worker, &backend, &mut state).await; }
                        }
                    }
                }
            })?;
            state.worker_started = true;
        }
        ctx.host(
            "host.background.retain",
            json!({"key":RESOURCE, "durable":true,"data":{}}),
        )
        .await?;
        match self.refresh(ctx, backend, &mut state).await {
            Ok(_) => {
                state.active = true;
                self.change.notify_one();
                Ok(json!({"ready":true}))
            }
            Err(error) => {
                let _ = root
                    .host("host.background.release", json!({"key":RESOURCE}))
                    .await;
                let _ = root
                    .host("host.tray.remove", json!({"resourceId":RESOURCE}))
                    .await;
                Err(error)
            }
        }
    }

    pub async fn event(
        &self,
        ctx: &ExtensionContext,
        backend: &Backend,
        payload: &Value,
    ) -> CoreResult {
        if payload["resourceId"] != RESOURCE {
            return Err("Unknown Arc menu resource".into());
        }
        let mut state = self.state.lock().await;
        if !state.active {
            return Err("Arc menu has been disabled".into());
        }
        let action = state
            .callbacks
            .get(text(payload, "actionId"))
            .cloned()
            .ok_or("Stale Arc menu action")?;
        match action {
            Callback::Arc(action) => {
                backend.run(ctx, action).await?;
            }
            Callback::Refresh => {
                return self.refresh(ctx, backend, &mut state).await;
            }
            Callback::Settings => {
                ctx.host(
                    "host.settings.open",
                    json!({"section":"settings-extensions"}),
                )
                .await?;
            }
            Callback::Disable => {
                ctx.host("host.background.release", json!({"key":RESOURCE}))
                    .await?;
                state.active = false;
                state.callbacks.clear();
                self.change.notify_one();
                ctx.host("host.tray.remove", json!({"resourceId":RESOURCE}))
                    .await?;
            }
        }
        Ok(json!({"ok":true}))
    }
}
