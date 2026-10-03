//! One stage on the screen. Text and a key line, no button grid.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::disk::{Disk, Link, human_bytes};
use crate::model::{App, CopyView, Role, Stage, WifiPhase};
use crate::speed::{format_rate, rate_is_fast};

const DIM: Color = Color::DarkGray;
const WARN: Color = Color::Red;
const OK: Color = Color::Green;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let body = inset(body);
    frame.render_widget(
        Paragraph::new(body_lines(app)).wrap(Wrap { trim: false }),
        body,
    );
    frame.render_widget(Paragraph::new(footer_line(app)), footer);
}

fn inset(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(2).min(area.right().saturating_sub(1)),
        y: area
            .y
            .saturating_add(1)
            .min(area.bottom().saturating_sub(1)),
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    }
}

fn body_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("omaclone", Style::new().bold()),
            Span::styled(format!("  {}", stage_name(app.stage)), Style::new().fg(DIM)),
        ]),
        Line::from(""),
    ];
    lines.extend(match app.stage {
        Stage::Role => role_lines(app),
        Stage::Disk => disk_lines(app),
        Stage::Cable => cable_lines(app),
        Stage::Confirm => confirm_lines(app),
        Stage::Ready => ready_lines(app),
        Stage::Wifi => wifi_lines(app),
    });
    lines
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Role => "Role",
        Stage::Disk => "Disk",
        Stage::Cable => "Cable",
        Stage::Confirm => "Confirm",
        Stage::Ready => "Ready",
        Stage::Wifi => "Wi-Fi",
    }
}

fn role_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("This laptop is one side of the copy."),
        Line::from("The other laptop boots the same stick and takes the other role."),
        Line::from(""),
    ];
    if !app.inventory.product.is_empty() {
        lines.push(dim_line(&app.inventory.product));
        lines.push(Line::from(""));
    }
    lines.push(key_line("s", "source", "this disk is read"));
    lines.push(Line::from(""));
    lines.push(key_line("t", "target", "this disk is erased"));
    lines.push(Line::from(""));
    lines.push(key_line("w", "wi-fi", "not required for cloning"));
    lines
}

fn disk_lines(app: &App) -> Vec<Line<'static>> {
    let role = role_word(app.role);
    let mut lines = vec![Line::from(format!("Role  {role}")), Line::from("")];
    match &app.inventory.disk {
        Ok(disk) => {
            lines.extend(disk_block(disk));
            if let Some(stick) = app.inventory.usb.iter().find(|usb| usb.label.is_some()) {
                lines.push(Line::from(""));
                lines.push(dim_line(&format!(
                    "Boot stick {} is not the disk above.",
                    stick.label.as_deref().unwrap_or("")
                )));
            }
        }
        Err(err) => {
            for line in err.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::new().fg(WARN),
                )));
            }
        }
    }
    lines
}

fn cable_lines(app: &App) -> Vec<Line<'static>> {
    let link = &app.inventory.link;
    let state = link.oper.as_deref().unwrap_or("down");
    let (addr, addr_ok) = match link.addr.as_deref() {
        Some(addr) if !addr.is_empty() => (addr.to_string(), true),
        _ => ("none".to_string(), false),
    };
    let (clx, clx_ok) = flag_parts(link.clx.as_deref());
    let (e2e, e2e_ok) = flag_parts(link.e2e.as_deref());
    let mut lines = vec![
        Line::from("Thunderbolt between the two laptops carries the bytes."),
        Line::from(""),
        cable_value("state", state, state == "up"),
        cable_value("address", &addr, addr_ok),
        cable_value("clx", &clx, clx_ok),
        cable_value("e2e", &e2e, e2e_ok),
        drops_line(app.recent_drops),
        speed_line(app),
        Line::from(""),
    ];
    if !app.link_note.is_empty() {
        let style = if app.link_note.starts_with("error:") {
            Style::new().fg(WARN)
        } else {
            Style::new().fg(DIM)
        };
        lines.push(Line::from(Span::styled(app.link_note.clone(), style)));
        lines.push(Line::from(""));
    }
    lines.push(dim_line(link_status(link)));
    lines.push(Line::from(""));
    lines.push(dim_line(&format!("{}  checking", checking_mark(app.pulse))));
    lines
}

fn confirm_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            "This disk will be erased.",
            Style::new().fg(WARN).bold(),
        )),
        Line::from(""),
    ];
    if let Ok(disk) = &app.inventory.disk {
        lines.extend(disk_block(disk));
        lines.push(Line::from(""));
        lines.push(Line::from("Type the serial."));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{}_", app.typed),
            Style::new().bold(),
        )));
        if app.mismatch {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "That is not the serial.",
                Style::new().fg(WARN),
            )));
        }
    }
    lines
}

fn ready_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match app.role {
        Some(Role::Source) => {
            lines.push(Line::from("Ready to read this disk."));
            if app.copy_view == CopyView::Idle {
                lines.push(dim_line("The target laptop has to type its own serial."));
            }
        }
        Some(Role::Target) => {
            lines.push(Line::from("Serial matches."));
            if matches!(app.copy_view, CopyView::Idle | CopyView::Waiting) {
                lines.push(Line::from(Span::styled(
                    "Ready to erase this disk when the copy starts.",
                    Style::new().fg(WARN),
                )));
            }
        }
        None => lines.push(Line::from("No role selected.")),
    }
    lines.push(Line::from(""));
    if !app.copy_peer.is_empty() && app.copy_view != CopyView::Idle {
        lines.push(dim_line(&format!("Other  {}", app.copy_peer)));
    }
    match app.copy_view {
        CopyView::Idle => {
            if !app.copy_note.is_empty() {
                lines.push(note_line(&app.copy_note, app.copy_view));
            }
            lines.push(dim_line("Press enter on both laptops to start."));
        }
        CopyView::Copying => {
            if app.copy_note == "Stopping." {
                lines.push(dim_line("Stopping."));
            }
            lines.push(Line::from(progress_text(app.copy_done, app.copy_total)));
        }
        CopyView::Done => {
            lines.push(note_line("Copy finished.", CopyView::Done));
            if !app.copy_hash.is_empty() {
                lines.push(dim_line(&format!("BLAKE3  {}", app.copy_hash)));
            }
            if app.copy_total > 0 {
                lines.push(dim_line(&progress_text(app.copy_done, app.copy_total)));
            }
        }
        CopyView::Waiting | CopyView::Stopped | CopyView::Failed => {
            if !app.copy_note.is_empty() {
                lines.push(note_line(&app.copy_note, app.copy_view));
            }
            if app.copy_view != CopyView::Waiting && app.copy_total > 0 {
                lines.push(dim_line(&progress_text(app.copy_done, app.copy_total)));
            }
        }
    }
    lines.push(Line::from(""));
    if let Ok(disk) = &app.inventory.disk {
        lines.extend(disk_block(disk));
    }
    lines
}

fn progress_text(done: u64, total: u64) -> String {
    let pct = if total == 0 {
        0
    } else {
        done.saturating_mul(100) / total
    };
    format!(
        "{pct:>3}%    {} of {}",
        human_bytes(done),
        human_bytes(total)
    )
}

fn note_line(note: &str, view: CopyView) -> Line<'static> {
    let style = match view {
        CopyView::Done => Style::new().fg(OK),
        CopyView::Failed | CopyView::Stopped => Style::new().fg(WARN),
        CopyView::Idle if note.starts_with("error:") => Style::new().fg(WARN),
        _ => Style::new().fg(DIM),
    };
    Line::from(Span::styled(note.to_string(), style))
}

fn disk_block(disk: &Disk) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(disk.model.clone()),
        Line::from(format!(
            "{}    {} bytes",
            human_bytes(disk.bytes),
            disk.bytes
        )),
        Line::from(Span::styled(disk.serial.clone(), Style::new().bold())),
        dim_line(&format!("/dev/{}", disk.name)),
    ];
    for part in disk.parts.iter().take(6) {
        lines.push(dim_line(&format!(
            "{}    {}",
            part.name,
            human_bytes(part.bytes)
        )));
    }
    lines
}

fn link_status(link: &Link) -> &'static str {
    if link.passes_frames() {
        "Thunderbolt is connected."
    } else if link.copy_block() == Some("traffic will not pass. Press r on both laptops.") {
        "The link is up, but traffic will not pass. Press r on both laptops."
    } else if link.addr.is_none() && link.clx.is_none() && link.e2e.is_none() {
        "Thunderbolt is not ready. Connect the cable between the two laptops."
    } else {
        "Thunderbolt is not ready. Try reconnecting the cable."
    }
}

fn checking_mark(pulse: u8) -> char {
    match pulse % 4 {
        0 => '|',
        1 => '/',
        2 => '-',
        _ => '\\',
    }
}

fn flag_parts(value: Option<&str>) -> (String, bool) {
    match value {
        Some("N" | "n" | "0") => ("off".to_string(), true),
        Some("Y" | "y" | "1") => ("on".to_string(), false),
        Some(other) => (other.to_string(), false),
        None => ("not loaded".to_string(), false),
    }
}

fn drops_line(drops: Option<u64>) -> Line<'static> {
    match drops {
        Some(0) => cable_value("drops", "0", true),
        Some(n) => cable_value("drops", &n.to_string(), false),
        None => cable_value("drops", "none", false),
    }
}

fn speed_line(app: &App) -> Line<'static> {
    if app.speed_testing {
        return cable_dim("speed", "testing");
    }
    if let Some(bits) = app.speed_bits {
        return cable_value("speed", &format_rate(bits), rate_is_fast(bits));
    }
    if !app.speed_note.is_empty() {
        return cable_dim("speed", &app.speed_note);
    }
    cable_dim("speed", "none")
}

fn cable_dim(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<8}"), Style::new().fg(DIM)),
        Span::styled(value.to_string(), Style::new().fg(DIM)),
    ])
}

fn cable_value(label: &str, value: &str, good: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<8}"), Style::new().fg(DIM)),
        Span::styled(
            value.to_string(),
            Style::new().fg(if good { OK } else { WARN }),
        ),
    ])
}

fn wifi_lines(app: &App) -> Vec<Line<'static>> {
    let wifi = &app.wifi;
    let mut lines = vec![
        dim_line("Rescue network. The copy does not use it."),
        Line::from(""),
    ];
    match wifi.phase {
        WifiPhase::List => {
            if !wifi.connected.is_empty() {
                lines.push(Line::from(Span::styled(
                    wifi.connected.clone(),
                    Style::new().fg(OK),
                )));
                lines.push(Line::from(""));
            }
            if !wifi.note.is_empty() {
                let style = if wifi.note.starts_with("error:") {
                    Style::new().fg(WARN)
                } else {
                    Style::new().fg(DIM)
                };
                lines.push(Line::from(Span::styled(wifi.note.clone(), style)));
                lines.push(Line::from(""));
            }
            if wifi.networks.is_empty() {
                lines.push(Line::from("No networks in the last scan."));
            }
            let start = wifi.cursor.saturating_sub(4);
            for (index, network) in wifi.networks.iter().enumerate().skip(start).take(10) {
                let mark = if index == wifi.cursor { ">" } else { " " };
                let security = if network.hidden() {
                    "hidden".to_string()
                } else if network.open() {
                    "open".to_string()
                } else {
                    network.security.clone()
                };
                let used = if network.in_use { "  connected" } else { "" };
                let style = if index == wifi.cursor {
                    Style::new().bold()
                } else {
                    Style::new()
                };
                lines.push(Line::from(Span::styled(
                    format!(
                        "{mark}  {:<24} {:>3}  {security}{used}",
                        network.label(),
                        network.signal
                    ),
                    style,
                )));
            }
        }
        WifiPhase::Ssid => {
            lines.push(Line::from("Hidden network"));
            lines.push(Line::from(""));
            lines.push(Line::from("Name"));
            lines.push(Line::from(Span::styled(
                format!("{}_", wifi.ssid),
                Style::new().bold(),
            )));
            if !wifi.note.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    wifi.note.clone(),
                    Style::new().fg(WARN),
                )));
            }
        }
        WifiPhase::Password => {
            lines.push(Line::from(wifi.ssid.clone()));
            lines.push(Line::from(""));
            lines.push(Line::from("Password"));
            let shown = if wifi.reveal {
                format!("{}_", wifi.password)
            } else if wifi.password.is_empty() {
                "_".to_string()
            } else {
                "•".repeat(wifi.password.chars().count())
            };
            lines.push(Line::from(Span::styled(shown, Style::new().bold())));
            if !wifi.note.is_empty() {
                lines.push(Line::from(""));
                lines.push(dim_line(&wifi.note));
            }
        }
    }
    lines
}

fn footer_line(app: &App) -> Line<'static> {
    let text = match app.stage {
        Stage::Role => "s source    t target    w wi-fi    q quit",
        Stage::Disk if app.inventory.disk.is_err() => "w wi-fi    esc back    q quit",
        Stage::Disk => "enter continue    w wi-fi    esc back    q quit",
        Stage::Cable => "t test    r reload    enter continue    w wi-fi    esc back    q quit",
        Stage::Confirm => "enter confirm    esc back    ctrl-c quit",
        Stage::Ready => match app.copy_view {
            CopyView::Waiting | CopyView::Copying => "esc abort",
            CopyView::Done => "esc back    q quit",
            _ => "enter start    w wi-fi    esc back    q quit",
        },
        Stage::Wifi => match app.wifi.phase {
            WifiPhase::List => {
                "j/k move    enter join    d disconnect    h hidden    s scan    esc back"
            }
            WifiPhase::Ssid => "enter continue    esc back",
            WifiPhase::Password => "tab show    enter connect    esc back",
        },
    };
    legend(text, !app.wifi.connected.is_empty())
}

fn legend(text: &str, connected: bool) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, group) in text.split("    ").enumerate() {
        if index > 0 {
            spans.push(Span::styled("    ", Style::new().fg(DIM)));
        }
        let mut parts = group.splitn(2, ' ');
        let key = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        spans.push(Span::styled(key.to_string(), Style::new().bold()));
        if !rest.is_empty() {
            let rest_style = if connected && rest == "wi-fi" {
                Style::new().fg(OK)
            } else {
                Style::new().fg(DIM)
            };
            spans.push(Span::styled(format!(" {rest}"), rest_style));
        }
    }
    Line::from(spans)
}

fn key_line(key: &str, name: &str, detail: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key}  "), Style::new().bold()),
        Span::raw(format!("{name:<8}")),
        Span::styled(detail.to_string(), Style::new().fg(DIM)),
    ])
}

fn dim_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), Style::new().fg(DIM)))
}

fn role_word(role: Option<Role>) -> &'static str {
    match role {
        Some(Role::Source) => "source",
        Some(Role::Target) => "target",
        None => "unset",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::{Disk, Inventory, Link, Part};
    use crate::model::{App, CopyView, Role, Stage, WifiPhase};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::{Color, Modifier};

    fn sample(stage: Stage, role: Option<Role>) -> App {
        let mut app = App::new(Inventory {
            product: "MacBookPro11,4".to_string(),
            disk: Ok(Disk {
                name: "sda".to_string(),
                model: "APPLE SSD SM0256G".to_string(),
                serial: "S29CNYDG898371".to_string(),
                bytes: 251_000_193_024,
                parts: vec![Part {
                    name: "sda1".to_string(),
                    bytes: 2_147_483_648,
                }],
            }),
            usb: vec![],
            link: Link {
                clx: Some("N".to_string()),
                e2e: Some("N".to_string()),
                addr: Some("169.254.251.250".to_string()),
                oper: Some("up".to_string()),
                drops: Some(0),
            },
        });
        app.stage = stage;
        app.role = role;
        app
    }

    fn render(app: &App) -> String {
        let backend = TestBackend::new(72, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut lines = Vec::new();
        for y in 0..buffer.area.height {
            let mut line = String::new();
            for x in 0..buffer.area.width {
                line.push_str(buffer[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }

    #[test]
    fn cable_screen_says_what_to_do_and_shows_checking() {
        let mut down = sample(Stage::Cable, None);
        down.inventory.link.clx = Some("Y".to_string());
        down.inventory.link.oper = Some("down".to_string());
        let text = render(&down);
        assert!(text.contains("Press r on both laptops."));
        assert!(text.contains("r reload"));
        assert!(text.contains("|  checking"));
        assert!(!text.contains("drops packets"));
        down.pulse = 1;
        assert!(render(&down).contains("/  checking"));

        let mut bare = sample(Stage::Cable, None);
        bare.inventory.link = Link::offline();
        let text = render(&bare);
        assert!(text.contains("Connect the cable between the two laptops."));

        let up = render(&sample(Stage::Cable, None));
        assert!(up.contains("Thunderbolt is connected."));
    }

    #[test]
    fn cable_values_are_green_only_for_a_working_setup() {
        let up = sample(Stage::Cable, None);
        assert_eq!(value_color(&up, "state"), Some(Color::Green));
        assert_eq!(value_color(&up, "address"), Some(Color::Green));
        assert_eq!(value_color(&up, "clx"), Some(Color::Green));
        assert_eq!(value_color(&up, "e2e"), Some(Color::Green));
        assert_eq!(label_color(&up, "state"), Some(Color::DarkGray));
        assert_eq!(label_color(&up, "clx"), Some(Color::DarkGray));
        assert_eq!(value_color(&up, "drops"), Some(Color::Green));
        assert_eq!(value_color(&up, "speed"), Some(Color::DarkGray));

        let mut down = sample(Stage::Cable, None);
        down.inventory.link = Link {
            clx: Some("Y".to_string()),
            e2e: None,
            addr: None,
            oper: Some("down".to_string()),
            drops: Some(4),
        };
        assert_eq!(value_color(&down, "state"), Some(Color::Red));
        assert_eq!(value_color(&down, "address"), Some(Color::Red));
        assert_eq!(value_color(&down, "clx"), Some(Color::Red));
        assert_eq!(value_color(&down, "e2e"), Some(Color::Red));
        down.recent_drops = Some(4);
        assert_eq!(value_color(&down, "drops"), Some(Color::Red));
        down.speed_bits = Some(800_000_000);
        assert_eq!(value_color(&down, "speed"), Some(Color::Red));
        assert!(render(&down).contains("800 Mbit/s"));
        down.speed_bits = Some(10_300_000_000);
        assert_eq!(value_color(&down, "speed"), Some(Color::Green));
        assert!(render(&down).contains("10.3 Gbit/s"));
        let text = render(&down);
        assert!(text.contains("down"));
        assert!(text.contains("none"));
        assert!(text.contains("on"));
        assert!(text.contains("not loaded"));
    }

    fn buffer_of(app: &App) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(72, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn line_text(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        let mut text = String::new();
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text
    }

    fn value_color(app: &App, label: &str) -> Option<Color> {
        let buffer = buffer_of(app);
        for y in 0..buffer.area.height {
            let text = line_text(&buffer, y);
            let Some(at) = text.find(label) else {
                continue;
            };
            return buffer[(at as u16 + 8, y)].style().fg;
        }
        None
    }

    fn label_color(app: &App, label: &str) -> Option<Color> {
        let buffer = buffer_of(app);
        for y in 0..buffer.area.height {
            let text = line_text(&buffer, y);
            let Some(at) = text.find(label) else {
                continue;
            };
            return buffer[(at as u16, y)].style().fg;
        }
        None
    }

    #[test]
    fn ready_screen_starts_the_copy_and_shows_progress() {
        let mut app = sample(Stage::Ready, Some(Role::Source));
        let idle = render(&app);
        assert!(idle.contains("Press enter on both laptops to start."));
        assert!(idle.contains("enter start"));
        assert!(!idle.contains("does not start"));

        app.copy_view = CopyView::Copying;
        app.copy_done = 50;
        app.copy_total = 100;
        let copying = render(&app);
        assert!(copying.contains("50%"));
        assert!(copying.contains("esc abort"));
        assert!(!copying.contains("enter start"));

        app.copy_view = CopyView::Done;
        app.copy_done = 100;
        app.copy_hash = "abc".to_string();
        let done = render(&app);
        assert!(done.contains("Copy finished."));
        assert!(done.contains("BLAKE3  abc"));
        assert!(done.contains("esc back"));
    }

    #[test]
    fn role_screen_is_two_lines_and_a_footer() {
        let text = render(&sample(Stage::Role, None));
        assert!(text.contains("s  source"));
        assert!(text.contains("t  target"));
        assert!(text.contains("not required for cloning"));
        assert!(text.contains("q quit"));
        assert!(!text.contains('┌'));
    }

    #[test]
    fn footer_keys_are_bold() {
        let backend = TestBackend::new(72, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, &sample(Stage::Role, None)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let y = buffer.area.height - 1;
        let key = &buffer[(0, y)];
        assert_eq!(key.symbol(), "s");
        assert!(key.style().add_modifier.contains(Modifier::BOLD));
        let word = &buffer[(2, y)];
        assert_eq!(word.symbol(), "s");
        assert!(!word.style().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn footer_wifi_is_green_when_connected() {
        let mut app = sample(Stage::Role, None);
        app.wifi.connected = "Connected  S&L".to_string();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let y = buffer.area.height - 1;
        let mut label = String::new();
        for x in 0..buffer.area.width {
            label.push_str(buffer[(x, y)].symbol());
        }
        let at = label.find("wi-fi").expect("wi-fi label") as u16;
        let key = &buffer[(at - 2, y)];
        assert_eq!(key.symbol(), "w");
        assert_ne!(key.style().fg, Some(Color::Green));
        for x in at..at + "wi-fi".len() as u16 {
            assert_eq!(buffer[(x, y)].style().fg, Some(Color::Green));
        }
    }

    #[test]
    fn confirm_screen_names_the_serial() {
        let text = render(&sample(Stage::Confirm, Some(Role::Target)));
        assert!(text.contains("This disk will be erased."));
        assert!(text.contains("S29CNYDG898371"));
        assert!(text.contains("enter confirm"));
        assert!(!text.contains("q quit"));
    }

    #[test]
    fn wifi_screen_lists_networks_without_boxes() {
        let mut app = sample(Stage::Wifi, None);
        app.wifi.phase = WifiPhase::List;
        app.wifi.connected = "Connected  S&L  192.168.2.13".to_string();
        app.wifi.networks = vec![crate::wifi::Network {
            ssid: "S&L".to_string(),
            signal: 74,
            security: "WPA2".to_string(),
            in_use: false,
        }];
        let text = render(&app);
        assert!(text.contains("S&L"));
        assert!(text.contains("Connected  S&L  192.168.2.13"));
        assert!(text.contains("d disconnect"));
        assert!(text.contains("h hidden"));
        assert!(text.contains("Rescue network"));
        assert!(!text.contains('┌'));
        let buffer = {
            let backend = TestBackend::new(72, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| draw(frame, &app)).unwrap();
            terminal.backend().buffer().clone()
        };
        let mut saw_green = false;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].style().fg == Some(Color::Green) {
                    saw_green = true;
                }
            }
        }
        assert!(saw_green);
    }
}
