use anyhow::Result;
use async_recursion::async_recursion;
use collections::HashSet;
use futures::future::join_all;
use gpui::{AppContext as _, AsyncWindowContext, Axis, Entity, Task, WeakEntity};
use project::Project;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ui::{App, Context, Window};
use util::ResultExt as _;

use db::{
    query,
    sqlez::{domain::Domain, statement::Statement, thread_safe_connection::ThreadSafeConnection},
    sqlez_macros::sql,
};
use workspace::{
    ItemHandle, ItemId, Member, Pane, PaneAxis, PaneGroup, SerializableItem as _, Workspace,
    WorkspaceDb, WorkspaceId,
};

use crate::{
    TerminalView, default_working_directory,
    terminal_panel::{TerminalPanel, new_terminal_pane},
};

const SHELL_TERMINAL_KIND: &str = "shell";
const REMOTE_TMUX_TERMINAL_KIND: &str = "remote_tmux";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SerializedTerminalSource {
    Shell,
    RemoteTmux(String),
}

pub(crate) fn serialize_pane_group(
    pane_group: &PaneGroup,
    active_pane: &Entity<Pane>,
    cx: &mut App,
) -> SerializedPaneGroup {
    build_serialized_pane_group(&pane_group.root, active_pane, cx)
}

fn build_serialized_pane_group(
    pane_group: &Member,
    active_pane: &Entity<Pane>,
    cx: &mut App,
) -> SerializedPaneGroup {
    match pane_group {
        Member::Axis(PaneAxis {
            axis,
            members,
            flexes,
            bounding_boxes: _,
        }) => SerializedPaneGroup::Group {
            axis: SerializedAxis(*axis),
            children: members
                .iter()
                .map(|member| build_serialized_pane_group(member, active_pane, cx))
                .collect::<Vec<_>>(),
            flexes: Some(flexes.lock().clone()),
        },
        Member::Pane(pane_handle) => {
            SerializedPaneGroup::Pane(serialize_pane(pane_handle, pane_handle == active_pane, cx))
        }
    }
}

fn serialize_pane(pane: &Entity<Pane>, active: bool, cx: &mut App) -> SerializedPane {
    let mut items_to_serialize = HashSet::default();
    let pane = pane.read(cx);
    let children = pane
        .items()
        .filter_map(|item| {
            let terminal_view = item.act_as::<TerminalView>(cx)?;
            if terminal_view.read(cx).terminal().read(cx).task().is_some() {
                None
            } else {
                let id = item.item_id().as_u64();
                items_to_serialize.insert(id);
                Some(id)
            }
        })
        .collect::<Vec<_>>();
    let active_item = pane
        .active_item()
        .map(|item| item.item_id().as_u64())
        .filter(|active_id| items_to_serialize.contains(active_id));

    let pinned_count = pane.pinned_count();
    SerializedPane {
        active,
        children,
        active_item,
        pinned_count,
    }
}

pub(crate) fn deserialize_terminal_panel(
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    database_id: WorkspaceId,
    serialized_panel: SerializedTerminalPanel,
    window: &mut Window,
    cx: &mut App,
) -> Task<anyhow::Result<Entity<TerminalPanel>>> {
    window.spawn(cx, async move |cx| {
        let terminal_panel = workspace.update_in(cx, |workspace, window, cx| {
            cx.new(|cx| TerminalPanel::new(workspace, window, cx))
        })?;
        match &serialized_panel.items {
            SerializedItems::NoSplits(item_ids) => {
                let items = deserialize_terminal_views(
                    database_id,
                    project,
                    workspace,
                    item_ids.as_slice(),
                    cx,
                )
                .await;
                let active_item = serialized_panel.active_item_id;
                terminal_panel.update_in(cx, |terminal_panel, window, cx| {
                    terminal_panel.active_pane.update(cx, |pane, cx| {
                        populate_pane_items(pane, items, active_item, window, cx);
                    });
                })?;
            }
            SerializedItems::WithSplits(serialized_pane_group) => {
                let center_pane = deserialize_pane_group(
                    workspace,
                    project,
                    terminal_panel.clone(),
                    database_id,
                    serialized_pane_group,
                    cx,
                )
                .await;
                if let Some((center_group, active_pane)) = center_pane {
                    terminal_panel.update(cx, |terminal_panel, _| {
                        terminal_panel.center = PaneGroup::with_root(center_group);
                        terminal_panel.active_pane =
                            active_pane.unwrap_or_else(|| terminal_panel.center.first_pane());
                    });
                }
            }
        }

        Ok(terminal_panel)
    })
}

fn populate_pane_items(
    pane: &mut Pane,
    items: Vec<Entity<TerminalView>>,
    active_item: Option<u64>,
    window: &mut Window,
    cx: &mut Context<Pane>,
) {
    let mut active_item_index = None;
    for (item_index, item) in (pane.items_len()..).zip(items) {
        if Some(item.item_id().as_u64()) == active_item {
            active_item_index = Some(item_index);
        }
        pane.add_item(Box::new(item), false, false, None, window, cx);
    }
    if let Some(index) = active_item_index {
        pane.activate_item(index, false, false, window, cx);
    }
}

#[async_recursion(?Send)]
async fn deserialize_pane_group(
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    panel: Entity<TerminalPanel>,
    workspace_id: WorkspaceId,
    serialized: &SerializedPaneGroup,
    cx: &mut AsyncWindowContext,
) -> Option<(Member, Option<Entity<Pane>>)> {
    match serialized {
        SerializedPaneGroup::Group {
            axis,
            flexes,
            children,
        } => {
            let mut current_active_pane = None;
            let mut members = Vec::new();
            for child in children {
                if let Some((new_member, active_pane)) = deserialize_pane_group(
                    workspace.clone(),
                    project.clone(),
                    panel.clone(),
                    workspace_id,
                    child,
                    cx,
                )
                .await
                {
                    members.push(new_member);
                    current_active_pane = current_active_pane.or(active_pane);
                }
            }

            if members.is_empty() {
                return None;
            }

            if members.len() == 1 {
                return Some((members.remove(0), current_active_pane));
            }

            Some((
                Member::Axis(PaneAxis::load(axis.0, members, flexes.clone())),
                current_active_pane,
            ))
        }
        SerializedPaneGroup::Pane(serialized_pane) => {
            let active = serialized_pane.active;

            let pane = panel
                .update_in(cx, |terminal_panel, window, cx| {
                    new_terminal_pane(
                        workspace.clone(),
                        project.clone(),
                        terminal_panel.active_pane.read(cx).is_zoomed(),
                        window,
                        cx,
                    )
                })
                .log_err()?;
            let active_item = serialized_pane.active_item;
            let pinned_count = serialized_pane.pinned_count;
            let new_items = deserialize_terminal_views(
                workspace_id,
                project.clone(),
                workspace.clone(),
                serialized_pane.children.as_slice(),
                cx,
            );
            cx.spawn({
                let pane = pane.downgrade();
                async move |cx| {
                    let new_items = new_items.await;

                    let items = pane.update_in(cx, |pane, window, cx| {
                        populate_pane_items(pane, new_items, active_item, window, cx);
                        pane.set_pinned_count(pinned_count.min(pane.items_len()));
                        pane.items_len()
                    });
                    // Avoid blank panes in splits
                    if items.is_ok_and(|items| items == 0) {
                        let working_directory = workspace
                            .update(cx, |workspace, cx| default_working_directory(workspace, cx))
                            .ok()
                            .flatten();
                        let terminal = project
                            .update(cx, |project, cx| {
                                project.create_terminal_shell(working_directory, cx)
                            })
                            .await
                            .log_err();
                        let Some(terminal) = terminal else {
                            return;
                        };
                        pane.update_in(cx, |pane, window, cx| {
                            let terminal_view = Box::new(cx.new(|cx| {
                                TerminalView::new(
                                    terminal,
                                    workspace.clone(),
                                    Some(workspace_id),
                                    project.downgrade(),
                                    window,
                                    cx,
                                )
                            }));
                            pane.add_item(terminal_view, true, false, None, window, cx);
                        })
                        .ok();
                    }
                }
            })
            .await;
            Some((Member::Pane(pane.clone()), active.then_some(pane)))
        }
    }
}

fn deserialize_terminal_views(
    workspace_id: WorkspaceId,
    project: Entity<Project>,
    workspace: WeakEntity<Workspace>,
    item_ids: &[u64],
    cx: &mut AsyncWindowContext,
) -> impl Future<Output = Vec<Entity<TerminalView>>> + use<> {
    let deserialized_items = join_all(item_ids.iter().filter_map(|item_id| {
        cx.update(|window, cx| {
            TerminalView::deserialize(
                project.clone(),
                workspace.clone(),
                workspace_id,
                *item_id,
                window,
                cx,
            )
        })
        .ok()
    }));
    async move {
        deserialized_items
            .await
            .into_iter()
            .filter_map(|item| item.log_err())
            .collect()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SerializedTerminalPanel {
    pub items: SerializedItems,
    // A deprecated field, kept for backwards compatibility for the code before terminal splits were introduced.
    pub active_item_id: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SerializedItems {
    // The data stored before terminal splits were introduced.
    NoSplits(Vec<u64>),
    WithSplits(SerializedPaneGroup),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum SerializedPaneGroup {
    Pane(SerializedPane),
    Group {
        axis: SerializedAxis,
        flexes: Option<Vec<f32>>,
        children: Vec<SerializedPaneGroup>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SerializedPane {
    pub active: bool,
    pub children: Vec<u64>,
    pub active_item: Option<u64>,
    #[serde(default)]
    pub pinned_count: usize,
}

#[derive(Debug)]
pub(crate) struct SerializedAxis(pub Axis);

impl Serialize for SerializedAxis {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Axis::Horizontal => serializer.serialize_str("horizontal"),
            Axis::Vertical => serializer.serialize_str("vertical"),
        }
    }
}

impl<'de> Deserialize<'de> for SerializedAxis {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "horizontal" => Ok(SerializedAxis(Axis::Horizontal)),
            "vertical" => Ok(SerializedAxis(Axis::Vertical)),
            invalid => Err(serde::de::Error::custom(format!(
                "Invalid axis value: '{invalid}'"
            ))),
        }
    }
}

pub struct TerminalDb(ThreadSafeConnection);

impl Domain for TerminalDb {
    const NAME: &str = stringify!(TerminalDb);

    const MIGRATIONS: &[&str] = &[
        sql!(
            CREATE TABLE terminals (
                workspace_id INTEGER,
                item_id INTEGER UNIQUE,
                working_directory BLOB,
                PRIMARY KEY(workspace_id, item_id),
                FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                ON DELETE CASCADE
            ) STRICT;
        ),
        // Remove the unique constraint on the item_id table
        // SQLite doesn't have a way of doing this automatically, so
        // we have to do this silly copying.
        sql!(
            CREATE TABLE terminals2 (
                workspace_id INTEGER,
                item_id INTEGER,
                working_directory BLOB,
                PRIMARY KEY(workspace_id, item_id),
                FOREIGN KEY(workspace_id) REFERENCES workspaces(workspace_id)
                ON DELETE CASCADE
            ) STRICT;

            INSERT INTO terminals2 (workspace_id, item_id, working_directory)
            SELECT workspace_id, item_id, working_directory FROM terminals;

            DROP TABLE terminals;

            ALTER TABLE terminals2 RENAME TO terminals;
        ),
        sql! (
            ALTER TABLE terminals ADD COLUMN working_directory_path TEXT;
            UPDATE terminals SET working_directory_path = CAST(working_directory AS TEXT);
        ),
        sql! (
            ALTER TABLE terminals ADD COLUMN custom_title TEXT;
        ),
        sql! (
            ALTER TABLE terminals ADD COLUMN terminal_kind TEXT NOT NULL DEFAULT "shell";
            ALTER TABLE terminals ADD COLUMN remote_tmux_session_name TEXT;
        ),
    ];
}

db::static_connection!(TerminalDb, [WorkspaceDb]);

impl TerminalDb {
    query! {
       pub async fn update_workspace_id(
            new_id: WorkspaceId,
            old_id: WorkspaceId,
            item_id: ItemId
        ) -> Result<()> {
            UPDATE terminals
            SET workspace_id = ?
            WHERE workspace_id = ? AND item_id = ?
        }
    }

    pub async fn save_working_directory(
        &self,
        item_id: ItemId,
        workspace_id: WorkspaceId,
        working_directory: PathBuf,
    ) -> Result<()> {
        log::debug!(
            "Saving working directory {working_directory:?} for item {item_id} in workspace {workspace_id:?}"
        );
        let query =
            "INSERT INTO terminals(item_id, workspace_id, working_directory, working_directory_path)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT DO UPDATE SET
                item_id = ?1,
                workspace_id = ?2,
                working_directory = ?3,
                working_directory_path = ?4"
        ;
        self.write(move |conn| {
            let mut statement = Statement::prepare(conn, query)?;
            let mut next_index = statement.bind(&item_id, 1)?;
            next_index = statement.bind(&workspace_id, next_index)?;
            next_index = statement.bind(&working_directory, next_index)?;
            statement.bind(
                &working_directory.to_string_lossy().into_owned(),
                next_index,
            )?;
            statement.exec()
        })
        .await
    }

    query! {
        pub fn get_working_directory(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<PathBuf>> {
            SELECT working_directory
            FROM terminals
            WHERE item_id = ? AND workspace_id = ?
        }
    }

    pub async fn save_custom_title(
        &self,
        item_id: ItemId,
        workspace_id: WorkspaceId,
        custom_title: Option<String>,
    ) -> Result<()> {
        log::debug!(
            "Saving custom title {:?} for item {} in workspace {:?}",
            custom_title,
            item_id,
            workspace_id
        );
        self.write(move |conn| {
            let query = "INSERT INTO terminals (item_id, workspace_id, custom_title)
                VALUES (?1, ?2, ?3)
                ON CONFLICT (workspace_id, item_id) DO UPDATE SET
                    custom_title = excluded.custom_title";
            let mut statement = Statement::prepare(conn, query)?;
            let mut next_index = statement.bind(&item_id, 1)?;
            next_index = statement.bind(&workspace_id, next_index)?;
            statement.bind(&custom_title, next_index)?;
            statement.exec()
        })
        .await
    }

    query! {
        pub fn get_custom_title(item_id: ItemId, workspace_id: WorkspaceId) -> Result<Option<String>> {
            SELECT custom_title
            FROM terminals
            WHERE item_id = ? AND workspace_id = ?
        }
    }

    pub async fn save_terminal_source(
        &self,
        item_id: ItemId,
        workspace_id: WorkspaceId,
        remote_tmux_session_name: Option<String>,
    ) -> Result<()> {
        let terminal_kind = if remote_tmux_session_name.is_some() {
            REMOTE_TMUX_TERMINAL_KIND
        } else {
            SHELL_TERMINAL_KIND
        };
        self.write(move |conn| {
            let query = "INSERT INTO terminals
                (item_id, workspace_id, terminal_kind, remote_tmux_session_name)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT (workspace_id, item_id) DO UPDATE SET
                    terminal_kind = excluded.terminal_kind,
                    remote_tmux_session_name = excluded.remote_tmux_session_name";
            let mut statement = Statement::prepare(conn, query)?;
            let mut next_index = statement.bind(&item_id, 1)?;
            next_index = statement.bind(&workspace_id, next_index)?;
            next_index = statement.bind(&terminal_kind, next_index)?;
            statement.bind(&remote_tmux_session_name, next_index)?;
            statement.exec()
        })
        .await
    }

    query! {
        fn get_terminal_source_row(
            item_id: ItemId,
            workspace_id: WorkspaceId
        ) -> Result<Option<(String, Option<String>)>> {
            SELECT terminal_kind, remote_tmux_session_name
            FROM terminals
            WHERE item_id = ? AND workspace_id = ?
        }
    }

    pub fn get_terminal_source(
        &self,
        item_id: ItemId,
        workspace_id: WorkspaceId,
    ) -> Result<Option<SerializedTerminalSource>> {
        let Some((terminal_kind, session_name)) =
            self.get_terminal_source_row(item_id, workspace_id)?
        else {
            return Ok(None);
        };
        match (terminal_kind.as_str(), session_name) {
            (SHELL_TERMINAL_KIND, _) => Ok(Some(SerializedTerminalSource::Shell)),
            (REMOTE_TMUX_TERMINAL_KIND, Some(session_name)) => {
                Ok(Some(SerializedTerminalSource::RemoteTmux(session_name)))
            }
            (REMOTE_TMUX_TERMINAL_KIND, None) => {
                anyhow::bail!("remote tmux terminal is missing its session name")
            }
            (unknown, _) => anyhow::bail!("unknown terminal kind {unknown:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn terminal_db(test_name: &'static str) -> TerminalDb {
        let db = TerminalDb(db::open_test_db::<db::AppMigrator>(test_name).await);
        db.write(|conn| {
            Statement::prepare(conn, "INSERT INTO workspaces(workspace_id) VALUES (1)")?.exec()
        })
        .await
        .expect("create test workspace");
        db
    }

    #[gpui::test]
    async fn terminal_source_round_trips_and_defaults_to_shell() {
        let db = terminal_db("terminal_source_round_trips_and_defaults_to_shell").await;
        let workspace_id = WorkspaceId::from_i64(1);

        db.save_custom_title(1, workspace_id, None)
            .await
            .expect("create legacy-style terminal row");
        assert_eq!(
            db.get_terminal_source(1, workspace_id).unwrap(),
            Some(SerializedTerminalSource::Shell)
        );

        db.save_terminal_source(1, workspace_id, Some("api worker".to_string()))
            .await
            .expect("save remote tmux source");
        assert_eq!(
            db.get_terminal_source(1, workspace_id).unwrap(),
            Some(SerializedTerminalSource::RemoteTmux(
                "api worker".to_string()
            ))
        );

        db.save_terminal_source(1, workspace_id, None)
            .await
            .expect("restore shell source");
        assert_eq!(
            db.get_terminal_source(1, workspace_id).unwrap(),
            Some(SerializedTerminalSource::Shell)
        );
    }

    #[gpui::test]
    async fn invalid_remote_tmux_source_never_becomes_a_shell() {
        let db = terminal_db("invalid_remote_tmux_source_never_becomes_a_shell").await;
        let workspace_id = WorkspaceId::from_i64(1);
        db.save_custom_title(1, workspace_id, None)
            .await
            .expect("create terminal row");
        db.write(|conn| {
            Statement::prepare(
                conn,
                "UPDATE terminals SET terminal_kind = 'remote_tmux', \
                 remote_tmux_session_name = NULL WHERE workspace_id = 1 AND item_id = 1",
            )?
            .exec()
        })
        .await
        .expect("corrupt terminal source");

        assert!(db.get_terminal_source(1, workspace_id).is_err());
    }
}
