use ksni::{Handle, Icon, ToolTip, TrayMethods};
use std::process::Command;

pub struct OpsTray {
    pub ui_url: String,
    pub unhealthy: usize,
    pub nodes: usize,
    pub summary: String,
}

impl OpsTray {
    pub fn new(ui_url: String) -> Self {
        Self {
            ui_url,
            unhealthy: 0,
            nodes: 0,
            summary: "starting…".into(),
        }
    }
}

impl ksni::Tray for OpsTray {
    fn id(&self) -> String {
        "relay-ops".into()
    }

    fn title(&self) -> String {
        "Relay ops".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn status(&self) -> ksni::Status {
        if self.unhealthy > 0 {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![pixmap(false)]
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        vec![pixmap(true)]
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: self.icon_pixmap(),
            title: "Relay ops".into(),
            description: self.summary.clone(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        open_ui(&self.ui_url);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let url = self.ui_url.clone();
        vec![
            StandardItem {
                label: "Open fleet UI".into(),
                activate: Box::new(move |_| open_ui(&url)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: format!(
                    "{} / {} nodes down",
                    self.unhealthy, self.nodes
                ),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ]
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true
    }
}

fn open_ui(url: &str) {
    let _ = Command::new("xdg-open").arg(url).spawn();
}

/// 32×32 ARGB32 cube + funnel. `attention` draws the red status dot.
pub fn pixmap(attention: bool) -> Icon {
    let n = 32i32;
    let mut data = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let (a, r, g, b) = pixel(x, y, attention);
            let i = ((y * n + x) * 4) as usize;
            data[i] = a;
            data[i + 1] = r;
            data[i + 2] = g;
            data[i + 3] = b;
        }
    }
    Icon {
        width: n,
        height: n,
        data,
    }
}

fn pixel(x: i32, y: i32, attention: bool) -> (u8, u8, u8, u8) {
    let dx = x - 15;
    let dy = y - 15;
    let in_cube = dx.abs() <= 11 && dy.abs() <= 11 && (dx.abs() + dy.abs()) <= 18;
    let in_funnel = y >= 12 && y <= 22 && {
        let half = (y - 12) / 2 + 1;
        (x - 16).abs() <= half
    };
    let in_dot = attention && {
        let ddx = x - 25;
        let ddy = y - 25;
        ddx * ddx + ddy * ddy <= 16
    };
    if in_dot {
        return (255, 220, 40, 40);
    }
    if in_funnel && in_cube {
        return (255, 20, 20, 24);
    }
    if in_cube {
        return (255, 236, 236, 240);
    }
    (0, 0, 0, 0)
}

#[derive(Clone, Debug, Default)]
pub struct TrayView {
    pub unhealthy: usize,
    pub nodes: usize,
    pub summary: String,
}

pub fn spawn_with_updates(
    ui_url: String,
) -> tokio::sync::mpsc::UnboundedSender<TrayView> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TrayView>();
    tokio::spawn(async move {
        let Some(handle) = spawn(&ui_url).await else {
            tracing::warn!("no tray icon (StatusNotifier / AppIndicator unavailable)");
            return;
        };
        while let Some(view) = rx.recv().await {
            handle
                .update(|t| {
                    t.unhealthy = view.unhealthy;
                    t.nodes = view.nodes;
                    t.summary = view.summary.clone();
                })
                .await;
        }
    });
    tx
}

async fn spawn(ui_url: &str) -> Option<Handle<OpsTray>> {
    let tray = OpsTray::new(ui_url.to_string());
    match tray.assume_sni_available(true).spawn().await {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!("tray icon: {e:#}");
            None
        }
    }
}

pub fn write_png(path: &std::path::Path, attention: bool) -> anyhow::Result<()> {
    let icon = pixmap(attention);
    let mut img = image::RgbaImage::new(icon.width as u32, icon.height as u32);
    for y in 0..icon.height as u32 {
        for x in 0..icon.width as u32 {
            let i = ((y * icon.width as u32 + x) * 4) as usize;
            // pixmap is ARGB; RgbaImage wants RGBA
            img.put_pixel(
                x,
                y,
                image::Rgba([
                    icon.data[i + 1],
                    icon.data[i + 2],
                    icon.data[i + 3],
                    icon.data[i],
                ]),
            );
        }
    }
    img.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixmap_is_32_argb() {
        let p = pixmap(true);
        assert_eq!(p.width, 32);
        assert_eq!(p.data.len(), 32 * 32 * 4);
        assert!(p.data.iter().any(|&b| b != 0));
    }
}
