//! Short Thunderbolt throughput check. The sink stays up so either laptop can press `t`.
//! A result under 5 Gbit/s is slow: a working link on this cable is about 10 Gbit/s.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::copy::{self, peer_is_link_local};
use crate::disk;

const TCP_PORT: u16 = 39442;
const UDP_PORT: u16 = 39443;
const PROBE_PORT: u16 = 39444;
/// Below this, the cable screen shows the rate in red.
pub const FAST_ENOUGH_BITS: u64 = 5_000_000_000;
const TEST_FOR: Duration = Duration::from_secs(2);

struct State {
    bits: Mutex<Option<u64>>,
    note: Mutex<String>,
    testing: AtomicBool,
    last_try: Mutex<Option<Instant>>,
    yielded: AtomicBool,
    sink_up: AtomicBool,
}

fn speed_state() -> &'static State {
    static STATE: OnceLock<State> = OnceLock::new();
    STATE.get_or_init(|| State {
        bits: Mutex::new(None),
        note: Mutex::new(String::new()),
        testing: AtomicBool::new(false),
        last_try: Mutex::new(None),
        yielded: AtomicBool::new(false),
        sink_up: AtomicBool::new(false),
    })
}

pub struct View {
    pub bits: Option<u64>,
    pub note: String,
    pub testing: bool,
}

pub fn view() -> View {
    let state = speed_state();
    View {
        bits: *lock(&state.bits),
        note: lock(&state.note).clone(),
        testing: state.testing.load(Ordering::Relaxed),
    }
}

pub fn clear() {
    let state = speed_state();
    *lock(&state.bits) = None;
    lock(&state.note).clear();
    *lock(&state.last_try) = None;
    state.yielded.store(false, Ordering::Relaxed);
}

/// Returns true when a measurement thread was started.
pub fn start(force_client: bool) -> bool {
    let state = speed_state();
    if !force_client && state.yielded.load(Ordering::Relaxed) {
        return false;
    }
    if !force_client {
        let mut last = lock(&state.last_try);
        if last.is_some_and(|tried| tried.elapsed() < Duration::from_secs(1)) {
            return false;
        }
        *last = Some(Instant::now());
    }
    if state
        .testing
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    let spawned = thread::Builder::new()
        .name("omaclone-speed".to_string())
        .spawn(move || {
            let state = speed_state();
            let outcome = run(force_client);
            match outcome {
                Ok(Some(bits)) => publish(bits),
                Ok(None) => {}
                Err(err) => {
                    *lock(&state.note) = err;
                    *lock(&state.bits) = None;
                }
            }
            state.testing.store(false, Ordering::Release);
        });
    match spawned {
        Ok(_) => true,
        Err(_) => {
            state.testing.store(false, Ordering::Release);
            false
        }
    }
}

pub fn rate_is_fast(bits: u64) -> bool {
    bits >= FAST_ENOUGH_BITS
}

pub fn format_rate(bits: u64) -> String {
    if bits >= 1_000_000_000 {
        format!("{:.1} Gbit/s", bits as f64 / 1_000_000_000.0)
    } else if bits >= 1_000_000 {
        format!("{:.0} Mbit/s", bits as f64 / 1_000_000.0)
    } else {
        format!("{bits} bit/s")
    }
}

pub fn bits_per_second(bytes: u64, elapsed: Duration) -> u64 {
    let millis = elapsed.as_millis().max(1) as u64;
    bytes.saturating_mul(8).saturating_mul(1000) / millis
}

pub fn parse_neigh(text: &str) -> Vec<Ipv4Addr> {
    let mut found = Vec::new();
    for line in text.lines() {
        let Some(first) = line.split_whitespace().next() else {
            continue;
        };
        let Ok(ip) = first.parse::<Ipv4Addr>() else {
            continue;
        };
        if !peer_is_link_local(IpAddr::V4(ip)) {
            continue;
        }
        if line.split_whitespace().any(|word| word == "FAILED") {
            continue;
        }
        found.push(ip);
    }
    found
}

fn publish(bits: u64) {
    let state = speed_state();
    *lock(&state.bits) = Some(bits);
    lock(&state.note).clear();
}

fn run(force_client: bool) -> Result<Option<u64>, String> {
    let link = disk::read_link();
    if !link.passes_frames() {
        return Err("Thunderbolt is not ready.".to_string());
    }
    let local: Ipv4Addr = link
        .addr
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|_| "Thunderbolt has no IPv4 address.".to_string())?;
    ensure_sink(local);
    let Some(peer) = discover(local) else {
        return if force_client {
            Err("no answer".to_string())
        } else {
            Ok(None)
        };
    };
    if !force_client && local > peer {
        speed_state().yielded.store(true, Ordering::Relaxed);
        return Ok(None);
    }
    match push(local, peer) {
        Ok(bits) => Ok(Some(bits)),
        Err(_) if !force_client => Ok(None),
        Err(err) => Err(err),
    }
}

fn ensure_sink(local: Ipv4Addr) {
    let state = speed_state();
    if state
        .sink_up
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    if thread::Builder::new()
        .name("omaclone-speed-sink".to_string())
        .spawn(move || {
            if let Err(err) = serve(local) {
                *lock(&speed_state().note) = err;
                speed_state().sink_up.store(false, Ordering::Release);
            }
        })
        .is_err()
    {
        state.sink_up.store(false, Ordering::Release);
    }
}

fn serve(local: Ipv4Addr) -> Result<(), String> {
    let listener = copy::listen_on(local, TCP_PORT)?;
    let udp = copy::udp_on(UDP_PORT)?;
    let mut buf = [0u8; 16];
    loop {
        if let Ok((n, src)) = udp.recv_from(&mut buf) {
            if n >= 4 && &buf[..4] == b"OMAS" && peer_is_link_local(src.ip()) {
                let _ = udp.send_to(b"OMAS", src);
            }
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if peer_is_link_local(peer.ip()) {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                    if let Some(bits) = drain(&mut stream) {
                        publish(bits);
                    }
                }
            }
            Err(err) if is_wait(&err) => {}
            Err(err) => return Err(err.to_string()),
        }
    }
}

fn drain(stream: &mut TcpStream) -> Option<u64> {
    let mut buf = vec![0u8; 1024 * 1024];
    let mut bytes = 0u64;
    let mut started: Option<Instant> = None;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if started.is_none() {
                    started = Some(Instant::now());
                }
                bytes += n as u64;
            }
            Err(err) if is_wait(&err) => {
                if started.is_some() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let elapsed = started.map(|start| start.elapsed())?;
    if bytes == 0 {
        None
    } else {
        Some(bits_per_second(bytes, elapsed))
    }
}

fn push(local: Ipv4Addr, peer: Ipv4Addr) -> Result<u64, String> {
    let mut stream = copy::connect_on(local, SocketAddrV4::new(peer, TCP_PORT))?;
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let buf = vec![0u8; 1024 * 1024];
    let started = Instant::now();
    let mut sent = 0u64;
    while started.elapsed() < TEST_FOR {
        match stream.write(&buf) {
            Ok(0) => break,
            Ok(n) => sent += n as u64,
            Err(err) if is_wait(&err) => break,
            Err(err) if sent == 0 => return Err(err.to_string()),
            Err(_) => break,
        }
    }
    let elapsed = started.elapsed();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    if sent == 0 {
        return Err("no answer".to_string());
    }
    Ok(bits_per_second(sent, elapsed))
}

fn discover(local: Ipv4Addr) -> Option<Ipv4Addr> {
    if let Some(ip) = neigh(local) {
        return Some(ip);
    }
    probe(local)
}

fn neigh(local: Ipv4Addr) -> Option<Ipv4Addr> {
    let output = Command::new("ip")
        .args(["neigh", "show", "dev", "thunderbolt0"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_neigh(&text).into_iter().find(|ip| *ip != local)
}

fn probe(local: Ipv4Addr) -> Option<Ipv4Addr> {
    let socket = copy::udp_on(PROBE_PORT).ok()?;
    let dest = SocketAddr::from((Ipv4Addr::new(169, 254, 255, 255), UDP_PORT));
    socket.send_to(b"OMAS", dest).ok()?;
    let mut buf = [0u8; 16];
    let deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < deadline {
        if let Ok((n, src)) = socket.recv_from(&mut buf) {
            if n >= 4
                && &buf[..4] == b"OMAS"
                && peer_is_link_local(src.ip())
                && src.ip() != IpAddr::V4(local)
            {
                if let IpAddr::V4(ip) = src.ip() {
                    return Some(ip);
                }
            }
        }
    }
    None
}

fn is_wait(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_format_and_five_gbit_is_the_slow_line() {
        assert!(rate_is_fast(10_300_000_000));
        assert!(rate_is_fast(FAST_ENOUGH_BITS));
        assert!(!rate_is_fast(800_000_000));
        assert_eq!(format_rate(10_300_000_000), "10.3 Gbit/s");
        assert_eq!(format_rate(800_000_000), "800 Mbit/s");
        assert_eq!(
            bits_per_second(1_250_000_000, Duration::from_secs(1)),
            10_000_000_000
        );
    }

    #[test]
    fn neigh_keeps_thunderbolt_link_local_only() {
        let text = "\
169.254.247.15 dev thunderbolt0 lladdr aa:bb:cc:dd:ee:ff REACHABLE
192.168.2.9 dev wlp3s0 lladdr 4c:32:75:8b:62:f5 REACHABLE
169.254.1.1 dev thunderbolt0 lladdr aa:bb FAILED
169.254.255.255 dev thunderbolt0 lladdr aa:bb REACHABLE
";
        assert_eq!(parse_neigh(text), vec![Ipv4Addr::new(169, 254, 247, 15)]);
    }
}
