use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// Show a native OK/Cancel dialog and return `true` only if the user chose `confirm_label`.
///
/// The dialog is drawn by the operating system outside the webview, so script running in
/// the webview cannot answer it. Commands that destroy data or reveal key material call
/// this before acting.
pub async fn confirm_native(
    app: &AppHandle,
    title: &str,
    message: &str,
    confirm_label: &str,
) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            confirm_label.to_string(),
            "Cancel".to_string(),
        ))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });
    rx.await.unwrap_or(false)
}
