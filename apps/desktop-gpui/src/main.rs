use std::{cell::Cell, path::PathBuf, rc::Rc};

use desktop_gpui::{contracts::LaneContext, ui::assets::Assets, workspace::WorkspaceView};
use desktop_runtime::Profile;
use gpui::{AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();
    let mut args = std::env::args_os().skip(1);
    let profile = match args.next() {
        Some(argument) if argument == "--help" => {
            println!(
                "Anarlog native foundation\nUsage: desktop-gpui [--profile DIRECTORY]\nDefault: $HOME/.anarlog-gpui-sandbox/library.sqlite\nUse only an isolated profile or a backup copy, never the shipping app's live profile."
            );
            return Ok(());
        }
        Some(argument) if argument == "--profile" => PathBuf::from(
            args.next()
                .ok_or_else(|| anyhow::anyhow!("--profile requires a directory"))?,
        ),
        Some(_) => anyhow::bail!("Unknown option. Use --help."),
        None => PathBuf::from(
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or_else(|| anyhow::anyhow!("No home directory; pass --profile DIRECTORY"))?,
        )
        .join(".anarlog-gpui-sandbox"),
    };
    anyhow::ensure!(args.next().is_none(), "Unexpected extra arguments");
    let (runtime, ready) = desktop_gpui::runtime_bridge::start(Profile {
        database: profile.join("library.sqlite"),
    })?;
    Application::new().with_assets(Assets).run(move |cx| {
        let closing = Rc::new(Cell::new(false));
        let closed = Rc::new(Cell::new(false));
        let quit_runtime = runtime.clone();
        let quit_closed = closed.clone();
        cx.on_app_quit(move |_| {
            let runtime = quit_runtime.clone();
            let already_closed = quit_closed.get();
            async move {
                if !already_closed && let Err(error) = runtime.shutdown().await {
                    tracing::error!(%error, "runtime shutdown failed");
                }
            }
        })
        .detach();
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1100.), px(760.)),
                    cx,
                ))),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Anarlog — Native local library".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let root = cx.new(|cx| {
                    WorkspaceView::new(
                        LaneContext {
                            runtime: runtime.clone(),
                        },
                        ready,
                        window,
                        cx,
                    )
                });
                let root_weak = root.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    if closing.get() {
                        return false;
                    }
                    let can_close = root_weak
                        .update(cx, |root, cx| root.can_close(cx))
                        .unwrap_or(true);
                    if !can_close {
                        return false;
                    }
                    closing.set(true);
                    let runtime = runtime.clone();
                    let root = root_weak.clone();
                    let closed = closed.clone();
                    cx.spawn(async move |cx| match runtime.shutdown().await {
                        Ok(()) => {
                            closed.set(true);
                            let _ = cx.update(|cx| cx.quit());
                        }
                        Err(error) => {
                            let _ = root.update(cx, |root, cx| {
                                root.set_status(format!("Shutdown failed: {error}"), cx)
                            });
                        }
                    })
                    .detach();
                    false
                });
                root
            },
        );
        if let Err(error) = result {
            tracing::error!(%error, "native window could not open");
            cx.quit();
        }
        cx.activate(true);
    });
    Ok(())
}
