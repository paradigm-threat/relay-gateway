use ksni::{Handle, Icon, ToolTip, TrayMethods};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub struct OpsTray {
    pub ui_url: String,
    pub unhealthy: usize,
    pub down: usize,
    pub sync: usize,
    pub nodes: usize,
    pub summary: String,
    pub mode: String,
    pub lines: Vec<String>,
    pub icon_path: String,
    pub attention_icon_path: String,
}

impl OpsTray {
    pub fn new(ui_url: String) -> Self {
        let (icon_path, attention_icon_path) = ensure_tray_icon_files();
        Self {
            ui_url,
            unhealthy: 0,
            down: 0,
            sync: 0,
            nodes: 0,
            summary: "starting…".into(),
            mode: "Observer".into(),
            lines: Vec::new(),
            icon_path,
            attention_icon_path,
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

    fn icon_name(&self) -> String {
        self.icon_path.clone()
    }

    fn attention_icon_name(&self) -> String {
        self.attention_icon_path.clone()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        pixmaps(false)
    }

    fn attention_icon_pixmap(&self) -> Vec<Icon> {
        pixmaps(true)
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
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: "Open fleet UI".into(),
                activate: Box::new(move |_| open_ui(&url)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: headline(self.down, self.nodes, self.sync, &self.mode),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ];
        for line in &self.lines {
            let url = self.ui_url.clone();
            items.push(
                StandardItem {
                    label: line.clone(),
                    activate: Box::new(move |_| open_ui(&url)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| request_quit()),
                ..Default::default()
            }
            .into(),
        );
        items
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true
    }
}

fn open_ui(url: &str) {
    let _ = Command::new("xdg-open").arg(url).spawn();
}

/// Stop the user service when we were started by systemd; otherwise exit the process.
/// `systemctl stop` is `--no-block` so we do not deadlock waiting for our own SIGTERM.
fn request_quit() {
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
    if under_systemd {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", "--no-block", "relay-ops.service"])
            .spawn();
        return;
    }
    std::process::exit(0);
}

/// Canonical Relay mark from `media/icon.png` (same 1024² PNG as client-web / Android).
const RELAY_ICON_PNG: &[u8] = include_bytes!("../../../media/icon.png");

const TRAY_SIZES: [u32; 2] = [32, 48];

pub fn pixmaps(attention: bool) -> Vec<Icon> {
    TRAY_SIZES.iter().map(|&n| pixmap_size(n, attention)).collect()
}

/// 32×32 ARGB32 Relay icon. `attention` draws the red status dot.
#[cfg_attr(not(test), allow(dead_code))]
pub fn pixmap(attention: bool) -> Icon {
    pixmap_size(32, attention)
}

fn pixmap_size(n: u32, attention: bool) -> Icon {
    let img = scaled_icon(n, attention);
    let mut data = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let p = img.get_pixel(x, y).0;
            let i = ((y * n + x) * 4) as usize;
            data[i] = p[3];
            data[i + 1] = p[0];
            data[i + 2] = p[1];
            data[i + 3] = p[2];
        }
    }
    Icon {
        width: n as i32,
        height: n as i32,
        data,
    }
}

fn source_rgba() -> &'static image::RgbaImage {
    static SRC: OnceLock<image::RgbaImage> = OnceLock::new();
    SRC.get_or_init(|| {
        let mut img = image::load_from_memory(RELAY_ICON_PNG)
            .expect("media/icon.png")
            .to_rgba8();
        punch_black(&mut img);
        crop_to_glyph(&img)
    })
}

fn crop_to_glyph(img: &image::RgbaImage) -> image::RgbaImage {
    let mut min_x = img.width();
    let mut min_y = img.height();
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[3] > 40 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if max_x <= min_x || max_y <= min_y {
        return img.clone();
    }
    let pad = ((max_x - min_x).max(max_y - min_y) / 16).max(8);
    let x0 = min_x.saturating_sub(pad);
    let y0 = min_y.saturating_sub(pad);
    let x1 = (max_x + pad + 1).min(img.width());
    let y1 = (max_y + pad + 1).min(img.height());
    image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image()
}

fn scaled_icon(n: u32, attention: bool) -> image::RgbaImage {
    let src = source_rgba();
    let (w, h) = src.dimensions();
    let inner = (n * 7 / 8).max(1);
    let scale = inner as f32 / w.max(h) as f32;
    let nw = ((w as f32 * scale).round() as u32).clamp(1, n);
    let nh = ((h as f32 * scale).round() as u32).clamp(1, n);
    let resized = image::imageops::resize(src, nw, nh, image::imageops::FilterType::Lanczos3);
    let mut img = image::RgbaImage::from_pixel(n, n, image::Rgba([0, 0, 0, 255]));
    let ox = i64::from((n - nw) / 2);
    let oy = i64::from((n - nh) / 2);
    image::imageops::overlay(&mut img, &resized, ox, oy);
    if attention {
        overlay_attention_dot(&mut img);
    }
    img
}

fn tray_icon_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("relay-ops"))
}

fn ensure_tray_icon_files() -> (String, String) {
    let Some(dir) = tray_icon_dir() else {
        return (String::new(), String::new());
    };
    let _ = std::fs::create_dir_all(&dir);
    let normal = dir.join("tray.png");
    let attention = dir.join("tray-attention.png");
    let _ = scaled_icon(64, false).save(&normal);
    let _ = scaled_icon(64, true).save(&attention);
    (
        path_if_file(&normal),
        path_if_file(&attention),
    )
}

fn path_if_file(path: &Path) -> String {
    if path.is_file() {
        path.display().to_string()
    } else {
        String::new()
    }
}

fn punch_black(img: &mut image::RgbaImage) {
    for p in img.pixels_mut() {
        let [r, g, b, _] = p.0;
        if r < 20 && g < 20 && b < 20 {
            p.0 = [0, 0, 0, 0];
        }
    }
}

fn overlay_attention_dot(img: &mut image::RgbaImage) {
    let w = img.width() as i32;
    let h = img.height() as i32;
    let r = (w.min(h) / 6).max(3);
    let cx = w - r - 1;
    let cy = h - r - 1;
    for y in 0..h {
        for x in 0..w {
            let dx = x - cx;
            let dy = y - cy;
            if dx * dx + dy * dy <= r * r {
                img.put_pixel(x as u32, y as u32, image::Rgba([220, 40, 40, 255]));
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TrayView {
    pub unhealthy: usize,
    pub down: usize,
    pub sync: usize,
    pub nodes: usize,
    pub summary: String,
    pub mode: String,
    pub lines: Vec<String>,
}

pub fn headline(down: usize, nodes: usize, sync: usize, mode: &str) -> String {
    if nodes == 0 {
        format!("{mode}  starting…")
    } else if down == 0 && sync == 0 {
        format!("{mode}  {nodes} up")
    } else if down == 0 {
        format!("{mode}  {nodes} up  ·  {sync} sync")
    } else if sync == 0 {
        format!("{mode}  {down} / {nodes} down")
    } else {
        format!("{mode}  {down} / {nodes} down  ·  {sync} sync")
    }
}

pub fn short_host(fqdn: &str) -> &str {
    fqdn.split('.').next().unwrap_or(fqdn)
}

pub fn node_line(
    fqdn: &str,
    healthy: bool,
    spot_ip: Option<&str>,
    dns: &[String],
    tcp443: bool,
    https_ok: bool,
    tcp_git: bool,
) -> String {
    let name = short_host(fqdn);
    let mismatch = spot_ip
        .map(|ip| !dns.is_empty() && !dns.iter().any(|d| d == ip))
        .unwrap_or(false);
    if healthy && !mismatch {
        return format!("{name}  up");
    }
    if healthy && mismatch {
        return format!("{name}  up  DNS≠Spot");
    }
    let mut bits = Vec::new();
    if mismatch {
        bits.push("DNS≠Spot");
    }
    if !tcp443 {
        bits.push("no https");
    } else if !https_ok {
        bits.push("no config");
    }
    if !tcp_git {
        bits.push("no git");
    }
    if bits.is_empty() {
        format!("{name}  DOWN")
    } else {
        format!("{name}  DOWN  {}", bits.join("  "))
    }
}

pub fn menu_lines(
    nodes: &[crate::probe::NodeProbe],
    fleet: &crate::topology::FleetFile,
    spot: &[crate::spot::SpotAssignment],
    ingress: Option<&crate::probe::IngressProbe>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for n in nodes {
        let https_ok = n.https_config.as_ref().map(|c| c.ok).unwrap_or(false);
        let spot_ip = fleet.node(&n.fqdn).and_then(|node| {
            node.spot_cloudspace.as_ref().and_then(|cs| {
                spot.iter()
                    .find(|s| s.cloudspace == *cs)
                    .map(|s| s.ip.as_str())
            })
        });
        lines.push({
            let mut line = node_line(
                &n.fqdn,
                n.healthy,
                spot_ip,
                &n.dns,
                n.tcp443,
                https_ok,
                n.tcp_git,
            );
            if let Some(c) = &n.https_config {
                let nrepos = if !c.repos.is_empty() {
                    c.repos.len()
                } else {
                    n.git_tips.len()
                };
                if nrepos > 0 {
                    line.push_str(&format!("  {nrepos} repos"));
                }
            } else if !n.git_tips.is_empty() {
                line.push_str(&format!("  {} repos", n.git_tips.len()));
            }
            if n.healthy && !n.sync_ok {
                if !n.missing_repos.is_empty() {
                    line.push_str(&format!("  missing {}", n.missing_repos.len()));
                }
                if !n.drift.is_empty() {
                    line.push_str(&format!("  drift {}", n.drift.len()));
                }
            }
            line
        });
    }
    if let Some(ing) = ingress {
        let keep = crate::probe::healthy_node_ips(nodes);
        let stale = crate::probe::dead_ingress_ips(ing, &keep);
        if stale.is_empty() {
            lines.push(format!("fleet  {} A", ing.dns.len()));
        } else {
            lines.push(format!("fleet  {} stale A", stale.len()));
        }
    }
    lines
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
                    t.down = view.down;
                    t.sync = view.sync;
                    t.nodes = view.nodes;
                    t.summary = view.summary.clone();
                    t.mode = view.mode.clone();
                    t.lines = view.lines.clone();
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
    write_png_size(path, 128, attention)
}

pub fn write_png_size(path: &std::path::Path, size: u32, attention: bool) -> anyhow::Result<()> {
    scaled_icon(size, attention).save(path)?;
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

    #[test]
    fn pixmap_uses_teal_relay_mark() {
        let p = pixmap(false);
        let mut teal = 0u32;
        let mut opaque = 0u32;
        for chunk in p.data.chunks_exact(4) {
            let (a, r, g, b) = (chunk[0], chunk[1], chunk[2], chunk[3]);
            if a > 200 {
                opaque += 1;
                if g > 120 && b > 120 && r < 80 {
                    teal += 1;
                }
            }
        }
        assert!(teal > 20, "expected cyan/teal Relay icon pixels, got {teal}");
        assert!(
            opaque > 100,
            "cropped logo should fill the tray tile, opaque={opaque}"
        );
    }

    #[test]
    fn writes_readable_preview() {
        let path = std::env::temp_dir().join("relay-ops-tray-64.png");
        scaled_icon(64, false).save(&path).unwrap();
        assert!(path.metadata().unwrap().len() > 200);
    }

    #[test]
    fn node_line_lon1_fresh_vm() {
        let line = node_line(
            "lon1.relaygateway.net",
            false,
            Some("134.213.138.108"),
            &["134.213.138.38".into()],
            false,
            false,
            false,
        );
        assert_eq!(line, "lon1  DOWN  DNS≠Spot  no https  no git");
    }

    #[test]
    fn headline_all_up() {
        assert_eq!(headline(0, 3, 0, "Admin"), "Admin  3 up");
        assert_eq!(headline(1, 3, 0, "Observer"), "Observer  1 / 3 down");
        assert_eq!(headline(0, 3, 1, "Admin"), "Admin  3 up  ·  1 sync");
    }
}
