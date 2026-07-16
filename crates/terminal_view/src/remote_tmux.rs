use editor::Editor;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    SharedString, WeakEntity, Window,
};
use menu::{Cancel, Confirm};
use project::terminals::RemoteTmuxSession;
use ui::prelude::*;
use workspace::{
    ModalView, Workspace,
    notifications::{DetachAndPromptErr, NotificationId},
};

use crate::terminal_panel::{AttachRemoteTmuxSession, TerminalPanel};

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, _: &mut Context<Workspace>| {
            workspace.register_action(show);
        },
    )
    .detach();
}

fn show(
    workspace: &mut Workspace,
    _: &AttachRemoteTmuxSession,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(host) = workspace.project().read(cx).ssh_remote_display_name(cx) else {
        workspace.show_toast(
            workspace::Toast::new(
                NotificationId::unique::<AttachRemoteTmuxSession>(),
                "Remote tmux sessions are only available in SSH remote projects",
            ),
            cx,
        );
        return;
    };

    let weak_workspace = cx.entity().downgrade();
    workspace.toggle_modal(window, cx, move |window, cx| {
        RemoteTmuxModal::new(weak_workspace, host, window, cx)
    });
}

struct RemoteTmuxModal {
    editor: Entity<Editor>,
    workspace: WeakEntity<Workspace>,
    host: SharedString,
    error: Option<SharedString>,
}

impl RemoteTmuxModal {
    fn new(
        workspace: WeakEntity<Workspace>,
        host: String,
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
            host: host.into(),
            error: None,
        }
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let session_name = self.editor.read(cx).text(cx);
        let session = match RemoteTmuxSession::new(session_name) {
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
        let Some(panel) = workspace.read(cx).panel::<TerminalPanel>(cx) else {
            return;
        };
        panel
            .update(cx, |panel, cx| {
                panel.add_remote_tmux_terminal(session, window, cx)
            })
            .detach_and_prompt_err(
                "Failed to attach remote tmux session",
                window,
                cx,
                |_, _, _| None,
            );
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for RemoteTmuxModal {}
impl ModalView for RemoteTmuxModal {}

impl Focusable for RemoteTmuxModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl Render for RemoteTmuxModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("RemoteTmuxModal")
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
                    .child(Headline::new("Attach Remote tmux Session").size(HeadlineSize::XSmall))
                    .child(
                        Label::new(format!("Attach an existing session on {}", self.host))
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
