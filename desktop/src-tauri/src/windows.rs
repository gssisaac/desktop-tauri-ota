use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const MAIN_WINDOW_LABEL: &str = "main";

pub fn focus_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Reload the page that is already open so a freshly started bundle replaces it.
pub fn reload_main_window(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or("main window is not open")?;
    window
        .eval("window.location.reload()")
        .map_err(|e| format!("reload main window: {e}"))
}

pub fn navigate_main_window(app: &AppHandle, url: tauri::Url) -> Result<(), String> {
    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or("main window is not open")?;
    window
        .navigate(url)
        .map_err(|e| format!("navigate main window: {e}"))
}

pub fn open_main_window(app: &AppHandle, webview_url: WebviewUrl) -> Result<(), String> {
    if app.get_webview_window(MAIN_WINDOW_LABEL).is_some() {
        return Ok(());
    }

    let mut builder = WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, webview_url)
        .title("__APP_NAME__")
        .inner_size(1280.0, 840.0)
        .min_inner_size(960.0, 640.0)
        .resizable(true)
        .decorations(true)
        .accept_first_mouse(true);

    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(tauri::LogicalPosition::new(14.0, 21.0));
    }

    builder
        .build()
        .map_err(|e| format!("create main window: {e}"))?;

    Ok(())
}
