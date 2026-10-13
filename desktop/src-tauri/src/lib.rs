mod api_bridge;
mod bundle_manager;
mod crypto;
mod startup;
mod windows;

use tauri::RunEvent;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::default().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            log::info!("second __APP_NAME__ instance requested; focusing existing window");
            windows::focus_main_window(app);
        }))
        .setup(|app| {
            let handle = app.handle().clone();

            // Debug: the Next dev server (devUrl). Release: the bundled launcher page, which
            // navigates to the full-stack server once `startup` has it running.
            if let Err(err) =
                windows::open_main_window(&handle, tauri::WebviewUrl::App("index.html".into()))
            {
                log::error!("failed to open main window: {err}");
            }

            if !cfg!(debug_assertions) {
                startup::spawn_boot(handle);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bundle_manager::ota_status,
            startup::launcher_status,
            startup::launcher_retry,
        ])
        .build(tauri::generate_context!())
        .expect("error while building __APP_NAME__")
        .run(|_app, event| {
            if matches!(
                event,
                RunEvent::Exit | RunEvent::ExitRequested { .. }
            ) {
                api_bridge::stop();
            }
        });
}
