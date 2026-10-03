mod copy;
mod disk;
mod model;
mod speed;
mod ui;
mod wifi;

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::copy::Running;
use crate::disk::{human_bytes, load_inventory};
use crate::model::{App, CopyView, Key, Role, WifiJob, WifiPhase};

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--print") {
        print_inventory(&load_inventory());
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--bench") {
        return bench_main(&args);
    }
    let inventory = load_inventory();
    if !io::stdout().is_terminal() {
        eprintln!("omaclone needs a terminal. Run with --print to show the disk and exit.");
        std::process::exit(1);
    }
    let mut app = App::new(inventory);
    show_link(&mut app, &wifi::snapshot());
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut session: Option<Running> = None;
    app.speed_addr = app.inventory.link.addr.clone();
    loop {
        sync_speed(app);
        if app.stage == crate::model::Stage::Ready {
            if let Some(running) = &session {
                copy::apply(app, &running.shared);
            }
        }
        terminal.draw(|frame| ui::draw(frame, app))?;
        if !event::poll(Duration::from_millis(200))? {
            let link = disk::read_link();
            if app.speed_addr != link.addr {
                app.speed_addr = link.addr.clone();
                app.speed_bits = None;
                app.speed_note.clear();
                speed::clear();
            }
            app.inventory.link = link;
            app.observe_drops(std::time::Instant::now());
            app.pulse = app.pulse.wrapping_add(1);
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if let Some(running) = session.take() {
                running.shared.request_abort();
                let _ = terminal.draw(|frame| ui::draw(frame, app));
                running.shutdown();
            }
            break;
        }
        let mapped = match key.code {
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Tab => Key::Tab,
            KeyCode::Char(ch) => Key::Char(ch),
            _ => continue,
        };
        let quit = app.on_key(mapped);
        if app.copy_abort {
            app.copy_abort = false;
            if let Some(running) = &session {
                running.shared.request_abort();
            }
        }
        if app.copy_reset {
            app.copy_reset = false;
            if session
                .as_ref()
                .is_none_or(|running| !running.shared.is_running())
            {
                if let Some(running) = session.take() {
                    running.shutdown();
                }
            }
        }
        if app.copy_launch {
            app.copy_launch = false;
            if session
                .as_ref()
                .is_some_and(|running| running.shared.is_running())
            {
                // The copy is already running.
            } else {
                if let Some(running) = session.take() {
                    running.shutdown();
                }
                match copy::start(app) {
                    Ok(running) => session = Some(running),
                    Err(err) => {
                        app.copy_view = CopyView::Failed;
                        app.copy_note = format!("error: {err}");
                    }
                }
            }
        }
        if quit {
            if let Some(running) = session.take() {
                running.shutdown();
            }
            break;
        }
        if app.reload {
            app.reload = false;
            terminal.draw(|frame| ui::draw(frame, app))?;
            match disk::reload_thunderbolt() {
                Ok(()) => app.link_note.clear(),
                Err(err) => app.link_note = format!("error: {err}"),
            }
            app.inventory.link = disk::read_link();
            continue;
        }
        if run_wifi(terminal, app)? {
            continue;
        }
    }
    Ok(())
}

fn sync_speed(app: &mut App) {
    let view = speed::view();
    app.speed_testing = view.testing;
    if view.testing {
        return;
    }
    if let Some(bits) = view.bits {
        app.speed_bits = Some(bits);
        app.speed_note.clear();
    } else if !view.note.is_empty() {
        app.speed_note = view.note;
        app.speed_bits = None;
    }
    if app.speed_again {
        app.speed_again = false;
        speed::clear();
        app.speed_bits = None;
        app.speed_note.clear();
        if speed::start(true) {
            app.speed_testing = true;
        }
        return;
    }
    if app.inventory.link.passes_frames()
        && app.speed_bits.is_none()
        && app.speed_note.is_empty()
        && !app.speed_testing
    {
        if speed::start(false) {
            app.speed_testing = true;
        }
    }
}

fn show_link(app: &mut App, link: &wifi::WifiLink) {
    if link.connected() {
        app.wifi_connected(&link.ssid, &link.ip);
    } else {
        app.wifi.connected.clear();
    }
}

fn run_wifi(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<bool> {
    let Some(job) = app.wifi.pending.take() else {
        return Ok(false);
    };
    terminal.draw(|frame| ui::draw(frame, app))?;
    match job {
        WifiJob::Scan => match wifi::scan() {
            Ok(networks) => {
                let link = wifi::snapshot();
                app.wifi.iface = link.iface.clone();
                app.wifi.networks = networks;
                app.wifi.cursor = 0;
                app.wifi.note.clear();
                app.wifi.phase = WifiPhase::List;
                show_link(app, &link);
            }
            Err(err) => {
                app.wifi.networks.clear();
                app.wifi.note = format!("error: {err}");
                app.wifi.phase = WifiPhase::List;
            }
        },
        WifiJob::Connect => {
            let ssid = app.wifi.ssid.clone();
            let password = app.wifi.password.clone();
            let hidden = app.wifi.hidden;
            let iface = app.wifi.iface.clone();
            match wifi::join(&iface, &ssid, &password, hidden) {
                Ok(link) => {
                    if link.iface.is_empty() {
                        app.wifi.iface = iface;
                    }
                    app.wifi_connected(&link.ssid, &link.ip);
                }
                Err(err) => {
                    app.wifi.note = format!("error: {err}");
                    if password.is_empty() && !hidden {
                        app.wifi.phase = WifiPhase::List;
                    } else {
                        app.wifi.phase = WifiPhase::Password;
                    }
                }
            }
        }
        WifiJob::Disconnect => match wifi::disconnect(&app.wifi.iface) {
            Ok(()) => app.wifi_disconnected(),
            Err(err) => {
                app.wifi.note = format!("error: {err}");
                app.wifi.phase = WifiPhase::List;
            }
        },
    }
    Ok(true)
}

fn bench_main(args: &[String]) -> io::Result<()> {
    let role = match args.get(1).map(String::as_str) {
        Some("source") => Role::Source,
        Some("target") => Role::Target,
        _ => {
            eprintln!("usage: omaclone --bench source|target [bytes]");
            std::process::exit(2);
        }
    };
    let copy_bytes = match args.get(2) {
        Some(text) => text.parse::<u64>().unwrap_or_else(|_| {
            eprintln!("bench length must be a byte count");
            std::process::exit(2);
        }),
        None => 8 * 1024 * 1024 * 1024,
    };
    let inventory = load_inventory();
    let disk = match &inventory.disk {
        Ok(disk) => disk,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "bench role={} serial={} disk={} copy={}",
        match role {
            Role::Source => "source",
            Role::Target => "target",
        },
        disk.serial,
        human_bytes(disk.bytes),
        human_bytes(copy_bytes)
    );
    match copy::run_bench(role, disk, copy_bytes) {
        Ok(report) => {
            let app_mib = report.bytes as f64 / report.seconds / (1024.0 * 1024.0);
            let app_gbit = report.bytes as f64 * 8.0 / report.seconds / 1_000_000_000.0;
            let wire = report.wire_tx.max(report.wire_rx);
            let wire_gbit = wire as f64 * 8.0 / report.seconds / 1_000_000_000.0;
            println!(
                "bench done bytes={} seconds={:.2} app={app_mib:.1} MiB/s app={app_gbit:.2} Gbit/s wire={wire_gbit:.2} Gbit/s hash={}",
                report.bytes, report.seconds, report.hash
            );
            Ok(())
        }
        Err(err) => {
            eprintln!("bench failed: {err}");
            std::process::exit(1);
        }
    }
}

fn print_inventory(inventory: &disk::Inventory) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{}", inventory.product);
    match &inventory.disk {
        Ok(disk) => {
            let _ = writeln!(out, "{}", disk.model);
            let _ = writeln!(out, "{}", disk.serial);
            let _ = writeln!(out, "{} bytes", disk.bytes);
            let _ = writeln!(out, "{}", human_bytes(disk.bytes));
            let _ = writeln!(out, "/dev/{}", disk.name);
        }
        Err(err) => {
            let _ = writeln!(out, "{err}");
        }
    }
    for usb in &inventory.usb {
        let label = usb.label.as_deref().unwrap_or("-");
        let _ = writeln!(out, "usb {} {} {label}", usb.name, usb.model);
    }
    let link = &inventory.link;
    let _ = writeln!(
        out,
        "thunderbolt clx={} e2e={} addr={} state={}",
        link.clx.as_deref().unwrap_or("-"),
        link.e2e.as_deref().unwrap_or("-"),
        link.addr.as_deref().unwrap_or("-"),
        link.oper.as_deref().unwrap_or("-"),
    );
}
