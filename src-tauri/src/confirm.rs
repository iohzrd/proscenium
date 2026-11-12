use proscenium_types::MAX_DEVICE_NAME_LEN;
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// `name` without control characters and at most `MAX_DEVICE_NAME_LEN` characters long,
/// for the text of a native dialog. Device names come from other devices; a line break
/// in one could add lines to the dialog text.
pub fn dialog_device_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEVICE_NAME_LEN)
        .collect()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialog_device_name_removes_control_characters() {
        assert_eq!(dialog_device_name("Linux"), "Linux");
        assert_eq!(dialog_device_name("a\n\nb\u{7}c"), "abc");
        assert_eq!(
            dialog_device_name(&"x".repeat(MAX_DEVICE_NAME_LEN + 5)).len(),
            MAX_DEVICE_NAME_LEN
        );
    }
}
