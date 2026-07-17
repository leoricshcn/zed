use client::ZED_URL_SCHEME;
use gpui::{AsyncApp, actions};

actions!(
    cli,
    [
        /// Registers the zed:// URL scheme handler.
        RegisterZedScheme
    ]
);

pub async fn register_zed_scheme(cx: &AsyncApp) -> anyhow::Result<()> {
    if release_channel::is_zed_tmux_build() {
        return Ok(());
    }

    cx.update(|cx| cx.register_url_scheme(ZED_URL_SCHEME)).await
}
