//! Arc tab and space identities belong to a single browser process and window.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tezbar_extension_sdk::ExtensionContext;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

const BRIDGE: &str = include_str!("arc.js");
const BUNDLE: &str = "company.thebrowser.Browser";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    Text(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WindowKey {
    pub process: String,
    pub window: Id,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Location {
    TopApp,
    Pinned,
    Unpinned,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Tab {
    pub id: Id,
    pub title: String,
    pub url: String,
    pub location: Location,
    pub spaces: Vec<Id>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Space {
    pub id: Id,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub key: Option<WindowKey>,
    pub active_space: Option<Id>,
    pub incognito: Option<bool>,
    pub spaces: Vec<Space>,
    pub tabs: Vec<Tab>,
}

impl Snapshot {
    pub fn space_label(&self, index: usize) -> String {
        let title = &self.spaces[index].title;
        if title.is_empty() {
            format!("Space {}", index + 1)
        } else {
            title.clone()
        }
    }

    pub fn named_space(&self, query: &str) -> Result<Option<Id>, String> {
        if query.is_empty() {
            return Ok(None);
        }
        let mut matches = self.spaces.iter().enumerate().filter(|(index, _)| {
            self.space_label(*index).to_lowercase() == query.to_lowercase()
                || (*index + 1).to_string() == query
        });
        let (_, space) = matches.next().ok_or("Arc space was not found")?;
        if matches.next().is_some() {
            return Err("More than one Arc space matches. Use its unique position number.".into());
        }
        Ok(Some(space.id.clone()))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TabAction {
    Select,
    Reload,
    Close,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Menu {
    BlankWindow,
    Easel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Operation {
    Version,
    Snapshot {
        launch: bool,
        tabs: bool,
    },
    Tab {
        key: WindowKey,
        tab: Id,
        action: TabAction,
        expected_url: Option<String>,
    },
    Space {
        key: WindowKey,
        space: Id,
    },
    NewTabs {
        key: WindowKey,
        space: Option<Id>,
        urls: Vec<String>,
    },
    NewWindow {
        scope: Option<WindowKey>,
        space: Option<Id>,
        incognito: bool,
        url: Option<String>,
    },
    Little {
        url: String,
    },
    Menu {
        menu: Menu,
    },
    Reset {
        key: WindowKey,
        space: Id,
    },
}

#[derive(Clone)]
pub enum Backend {
    Native,
    #[cfg(feature = "protocol-fixtures")]
    Fixture(PathBuf),
}

impl Backend {
    pub fn supported(&self, ctx: &ExtensionContext) -> Result<(), String> {
        match self {
            Self::Native
                if cfg!(target_os = "macos")
                    && [
                        "aarch64-apple-darwin",
                        "x86_64-apple-darwin",
                        "macOS",
                        "macos",
                    ]
                    .contains(&ctx.platform.as_str()) =>
            {
                Ok(())
            }
            Self::Native => Err("The Arc extension supports macOS only".into()),
            #[cfg(feature = "protocol-fixtures")]
            Self::Fixture(_) if ctx.platform == "arc-fixture" => Ok(()),
            #[cfg(feature = "protocol-fixtures")]
            Self::Fixture(_) => Err("Fixture executable requires the arc-fixture platform".into()),
        }
    }

    pub async fn check_installation(&self, ctx: &ExtensionContext) -> Result<(), String> {
        let reply = self.run(ctx, Operation::Version).await?;
        let version = reply
            .as_str()
            .ok_or("Arc could not be found. Install Arc to use these commands.")?;
        let parts: Vec<_> = version
            .split(['.', '-', '+'])
            .take(3)
            .map(str::parse::<u32>)
            .collect::<Result<_, _>>()
            .map_err(|_| "Could not read the installed Arc version")?;
        if parts.len() != 3 {
            return Err("Could not read the installed Arc version".into());
        }
        if parts.as_slice() < [0, 85, 0].as_slice()
            || (parts == [0, 85, 0]
                && version
                    .split('+')
                    .next()
                    .is_some_and(|prefix| prefix.contains('-')))
        {
            return Err("Arc 0.85.0 or newer is required. Check for updates in Arc.".into());
        }
        Ok(())
    }

    pub fn arc_dir(&self) -> Result<PathBuf, String> {
        match self {
            Self::Native => {
                let home = std::env::var_os("HOME").ok_or("macOS home directory is unavailable")?;
                Ok(PathBuf::from(home).join("Library/Application Support/Arc"))
            }
            #[cfg(feature = "protocol-fixtures")]
            Self::Fixture(path) => Ok(path.clone()),
        }
    }

    pub async fn snapshot(
        &self,
        ctx: &ExtensionContext,
        launch: bool,
        tabs: bool,
    ) -> Result<Snapshot, String> {
        serde_json::from_value(self.run(ctx, Operation::Snapshot { launch, tabs }).await?)
            .map_err(|_| "Arc returned invalid window, tab or space data".into())
    }

    pub async fn run(&self, ctx: &ExtensionContext, operation: Operation) -> Result<Value, String> {
        self.supported(ctx)?;
        match self {
            Self::Native => {
                let input = serde_json::to_vec(&operation).map_err(|e| e.to_string())?;
                let mut command = Command::new("/usr/bin/osascript");
                command.args(["-l", "JavaScript", "-e", BRIDGE]);
                let output = helper(
                    ctx,
                    command,
                    &input,
                    2 * 1024 * 1024,
                    Duration::from_secs(8),
                )
                .await?;
                serde_json::from_slice(&output)
                    .map_err(|_| "Arc automation returned invalid JSON".into())
            }
            #[cfg(feature = "protocol-fixtures")]
            Self::Fixture(path) => fixture_run(path, &operation),
        }
    }

    pub async fn open_in_arc(&self, ctx: &ExtensionContext, url: &str) -> Result<(), String> {
        self.supported(ctx)?;
        match self {
            Self::Native => {
                let mut command = Command::new("/usr/bin/open");
                command.args(["-b", BUNDLE, url]);
                helper(ctx, command, &[], 4096, Duration::from_secs(8)).await?;
            }
            #[cfg(feature = "protocol-fixtures")]
            Self::Fixture(path) => {
                fixture_journal(path, &serde_json::json!({"kind":"open-arc","url":url}))?
            }
        }
        Ok(())
    }
}

async fn read_bounded(stream: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    stream
        .take(limit as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|_| "Helper output could not be read")?;
    if data.len() > limit {
        return Err("Helper output exceeded its limit".into());
    }
    Ok(data)
}

pub async fn helper(
    ctx: &ExtensionContext,
    mut command: Command,
    input: &[u8],
    output_limit: usize,
    deadline: Duration,
) -> Result<Vec<u8>, String> {
    if input.len() > 128 * 1024 {
        return Err("Helper input exceeded its limit".into());
    }
    let work = async {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "Could not start the macOS helper")?;
        let mut stdin = child.stdin.take().ok_or("Missing helper input")?;
        let stdout = child.stdout.take().ok_or("Missing helper output")?;
        let stderr = child.stderr.take().ok_or("Missing helper error output")?;
        let write = async move {
            stdin
                .write_all(input)
                .await
                .map_err(|_| "Could not send helper request".to_owned())?;
            Ok(())
        };
        let (output, errors, _) = tokio::try_join!(
            read_bounded(stdout, output_limit),
            read_bounded(stderr, 32 * 1024),
            write
        )?;
        if !child
            .wait()
            .await
            .map_err(|_| "Could not wait for the macOS helper")?
            .success()
        {
            let error = String::from_utf8_lossy(&errors);
            if error.contains("ARC_STALE") {
                return Err("Arc window, tab or space changed. Refresh and try again.".into());
            }
            if error.contains("ARC_MISSING_MENU") {
                return Err("Arc menu item is unavailable. Check the Arc version, language and Accessibility permission.".into());
            }
            return Err("macOS helper failed. Check Arc installation and macOS Automation or Accessibility permissions for Arc commands.".into());
        }
        Ok(output)
    };
    tokio::select! {
        biased;
        _ = ctx.cancelled() => Err("Operation cancelled".into()),
        result = tokio::time::timeout(deadline, work) => result.map_err(|_| "macOS helper timed out. If an action was sent to Arc, its outcome may be uncertain.")?,
    }
}

#[cfg(feature = "protocol-fixtures")]
fn fixture_journal(path: &std::path::Path, value: &impl Serialize) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.join("actions.jsonl"))
        .map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    file.write_all(&bytes).map_err(|e| e.to_string())
}

#[cfg(feature = "protocol-fixtures")]
fn fixture_run(path: &std::path::Path, operation: &Operation) -> Result<Value, String> {
    if matches!(operation, Operation::Version) {
        return Ok(serde_json::json!("1.100.0"));
    }
    let raw = crate::data::read_file(&path.join("browser.json"), 2 * 1024 * 1024)?;
    let windows: Vec<Snapshot> = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let front = windows.first().cloned().unwrap_or_default();
    let find = |key: &WindowKey| windows.iter().find(|w| w.key.as_ref() == Some(key));
    let space_exists = |key: &WindowKey, space: &Id| {
        find(key).is_some_and(|w| w.spaces.iter().filter(|item| &item.id == space).count() == 1)
    };
    let valid = match operation {
        Operation::Version => unreachable!("handled before reading fixture"),
        Operation::Snapshot { launch, tabs } => {
            fixture_journal(path, operation)?;
            let mut data = front;
            if *launch && data.key.is_none() {
                return Err("Synthetic browser has no window".into());
            }
            if !tabs {
                data.tabs.clear();
            }
            return serde_json::to_value(data).map_err(|e| e.to_string());
        }
        Operation::Tab {
            key,
            tab,
            expected_url,
            ..
        } => find(key).is_some_and(|window| {
            let mut matches = window.tabs.iter().filter(|item| &item.id == tab);
            matches
                .next()
                .is_some_and(|item| expected_url.as_ref().is_none_or(|url| &item.url == url))
                && matches.next().is_none()
        }),
        Operation::Space { key, space } => space_exists(key, space),
        Operation::NewTabs { key, space, .. } => {
            find(key).is_some() && space.as_ref().is_none_or(|space| space_exists(key, space))
        }
        Operation::NewWindow { scope, space, .. } => space
            .as_ref()
            .is_none_or(|space| scope.as_ref().is_some_and(|key| space_exists(key, space))),
        Operation::Reset { key, space } => {
            front.key.as_ref() == Some(key) && front.active_space.as_ref() == Some(space)
        }
        Operation::Little { .. } | Operation::Menu { .. } => true,
    };
    if !valid {
        return Err("Arc window, tab or space changed. Refresh and try again.".into());
    }
    fixture_journal(path, operation)?;
    Ok(serde_json::json!({"done":true}))
}
