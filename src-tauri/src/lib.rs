mod config;
mod disk;
mod download;
mod ffmpeg;
mod job;
mod link;
mod manifest;
mod ops;

use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use job::{AppState, View};
use serde_json::Value;

#[tauri::command]
fn get_view(app: AppHandle) -> View {
    app.state::<AppState>().view()
}

#[tauri::command]
async fn pick_folder(app: AppHandle, title: String) -> Option<String> {
    app.dialog()
        .file()
        .set_title(title)
        .blocking_pick_folder()
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
}

/// The op's live check of the user's choices (e.g. disk space). Shape is the op's own.
#[tauri::command]
fn preflight(app: AppHandle, opts: Value) -> Result<Value, String> {
    job::preflight(&app, &opts)
}

#[tauri::command]
fn start(app: AppHandle, opts: Value) -> Result<(), String> {
    job::start(&app, opts)
}

#[tauri::command]
fn cancel(app: AppHandle) {
    job::cancel(&app);
}

#[tauri::command]
async fn resolve_partial(app: AppHandle, keep: bool) -> Result<(), String> {
    job::resolve_partial(&app, keep).await
}

#[tauri::command]
fn dismiss(app: AppHandle) {
    job::dismiss(&app);
}

/// Opens only the finished job's output, never an arbitrary path from the webview.
#[tauri::command]
fn open_output(app: AppHandle) -> Result<(), String> {
    let path = job::output_path(&app).ok_or("no finished job")?;
    app.opener().open_path(path.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

/// Opens the bundled third-party notices (FFmpeg's LGPL obligations).
#[tauri::command]
fn open_licenses(app: AppHandle) -> Result<(), String> {
    let path = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("licenses")
        .join("THIRD-PARTY-NOTICES.txt");
    app.opener().open_path(path.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

/// The window follows its content: the webview reports how many CSS pixels it is short
/// (positive) or over (negative), and the window's inner height changes by that much.
#[tauri::command]
fn fit_window(window: tauri::WebviewWindow, delta: f64) -> Result<(), String> {
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let inner = window.inner_size().map_err(|e| e.to_string())?.to_logical::<f64>(scale);
    let height = (inner.height + delta).clamp(160.0, 900.0).round();
    window
        .set_size(tauri::LogicalSize::new(inner.width, height))
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();

    // Must be registered first: on Windows/Linux a link starts a second process, which
    // forwards the link to the running one (deep-link feature) and exits.
    #[cfg(desktop)]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            job::focus_window(app);
        }));
    }

    builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            #[cfg(all(debug_assertions, any(windows, target_os = "linux")))]
            app.deep_link().register_all()?;

            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                if let Some(url) = event.urls().first() {
                    job::handle_link(&handle, url.as_str());
                }
            });
            if let Some(url) = app.deep_link().get_current()?.and_then(|u| u.into_iter().next()) {
                job::handle_link(app.handle(), url.as_str());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if job::is_running(window.app_handle()) {
                    api.prevent_close();
                    job::notice(window.app_handle(), "Cancel the job before closing Ranomaly Local.");
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_view,
            pick_folder,
            preflight,
            start,
            cancel,
            resolve_partial,
            dismiss,
            open_output,
            fit_window,
            open_licenses
        ])
        .run(tauri::generate_context!())
        .expect("error while running Ranomaly Local");
}
