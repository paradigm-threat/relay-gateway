use reqwest::Client;

pub async fn notify(webhook: Option<&str>, body: &str) {
    if let Some(url) = webhook.filter(|s| !s.is_empty()) {
        let client = Client::new();
        let _ = client
            .post(url)
            .json(&serde_json::json!({ "text": body }))
            .send()
            .await;
    }
    desktop(body);
}

fn desktop(body: &str) {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["Relay ops", body])
            .status();
    }
    #[cfg(target_os = "windows")]
    {
        let ps = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; Write-Output {}",
            powershell_quote(body)
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .status();
    }
}

#[cfg(target_os = "windows")]
fn powershell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
