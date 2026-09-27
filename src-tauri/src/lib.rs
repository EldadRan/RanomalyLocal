mod config;
mod disk;
mod download;
mod ffmpeg;
mod job;
mod link;
mod manifest;

use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use job::{AppState, StartOptions, View};

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

#[tauri::command]
fn check_disk(app: AppHandle, opts: StartOptions) -> Result<disk::DiskCheck, String> {
    job::check_disk(&app, &opts)
}

#[tauri::command]
fn start(app: AppHandle, opts: StartOptions) -> Result<(), String> {
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

/// Opens only the folder of the finished job, never an arbitrary path from the webview.
#[tauri::command]
fn open_output(app: AppHandle) -> Result<(), String> {
    let View::Done { frames_dir, .. } = app.state::<AppState>().view() else {
        return Err("no finished job".into());
    };
    app.opener().open_path(frames_dir, None::<&str>).map_err(|e| e.to_string())
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
                    job::notice(window.app_handle(), "Cancel the job before closing AA Ext.");
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_view,
            pick_folder,
            check_disk,
            start,
            cancel,
            resolve_partial,
            dismiss,
            open_output
        ])
        .run(tauri::generate_context!())
        .expect("error while running AA Ext");
}
