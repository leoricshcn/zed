use editor::Editor;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    SharedString, WeakEntity, Window,
};
use menu::{Cancel, Confirm};
use project::terminals::TmuxSession;
use ui::prelude::*;
use workspace::{
    ModalView, NewCenterTmux, Workspace,
    notifications::{DetachAndPromptErr, NotificationId},
};

use crate::terminal_panel::{AttachTmuxSession, TerminalPanel};

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _: &mut Context<Workspace>| {
            workspace.register_action(show_in_panel);
            workspace.register_action(show_in_center);
        },
    )
    .detach();
}

#[derive(Clone, Copy)]
enum TmuxTarget {
    Panel,
    Center,
}

impl TmuxTarget {
    fn notification_id(self) -> NotificationId {
        match self {
            Self::Panel => NotificationId::unique::<AttachTmuxSession>(),
            Self::Center => NotificationId::unique::<NewCenterTmux>(),
        }
    }
}

fn show_in_panel(
    workspace: &mut Workspace,
    _: &AttachTmuxSession,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show(workspace, TmuxTarget::Panel, window, cx);
}

fn show_in_center(
    workspace: &mut Workspace,
    _: &NewCenterTmux,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    show(workspace, TmuxTarget::Center, window, cx);
}

fn show(
    workspace: &mut Workspace,
    target: TmuxTarget,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(location) = workspace.project().read(cx).tmux_display_name(cx) else {
        workspace.show_toast(
            workspace::Toast::new(
                target.notification_id(),
                "tmux sessions are unavailable for this project",
            ),
            cx,
        );
        return;
    };

    let weak_workspace = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        TmuxModal::new(weak_workspace, location, target, window, cx)
    });
}

struct TmuxModal {
    editor: Entity<Editor>,
    workspace: WeakEntity<Workspace>,
    location: SharedString,
    target: TmuxTarget,
    error: Option<SharedString>,
}

impl TmuxModal {
    fn new(
        workspace: WeakEntity<Workspace>,
        location: String,
        target: TmuxTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Exact tmux session name", window, cx);
            editor
        });
        Self {
            editor,
            workspace,
            location: location.into(),
            target,
            error: None,
        }
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let session_name = self.editor.read(cx).text(cx);
        let session = match TmuxSession::new(session_name) {
            Ok(session) => session,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let task = match self.target {
            TmuxTarget::Panel => workspace
                .read(cx)
                .panel::<TerminalPanel>(cx)
                .map(|panel| {
                    panel.update(cx, |panel, cx| panel.add_tmux_terminal(session, window, cx))
                })
                .unwrap_or_else(|| {
                    gpui::Task::ready(Err(anyhow::anyhow!("terminal panel is unavailable")))
                }),
            TmuxTarget::Center => workspace.update(cx, |workspace, cx| {
                TerminalPanel::add_center_tmux_terminal(workspace, session, window, cx)
            }),
        };
        task.detach_and_prompt_err("Failed to attach tmux session", window, cx, |_, _, _| None);
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for TmuxModal {}
impl ModalView for TmuxModal {}

impl Focusable for TmuxModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for TmuxModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("TmuxModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(34.))
            .child(
                v_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .w_full()
                    .gap_1()
                    .child(Headline::new("Attach tmux Session").size(HeadlineSize::XSmall))
                    .child(
                        Label::new(format!("Attach an existing session on {}", self.location))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(div().px_3().pb_3().w_full().child(self.editor.clone()))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .mx_3()
                        .pb_3()
                        .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
            })
    }
}
