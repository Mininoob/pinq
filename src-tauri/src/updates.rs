//! Manual update check (Settings → System). The feed is `latest.json` on the
//! GitHub release, produced and signed by the release workflow. While the
//! repo is private the feed isn't reachable without auth — the check then
//! reports a friendly "no feed" instead of pretending to be up to date.

use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// Returns the available version, or None when already current.
#[tauri::command]
pub async fn check_updates(app: AppHandle) -> Result<Option<String>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(Some(update.version.clone())),
        Ok(None) => Ok(None),
        Err(e) => Err(format!("Update feed unreachable: {e}")),
    }
}

/// Download, install, and relaunch into the new version.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Err("Already up to date".into());
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}
