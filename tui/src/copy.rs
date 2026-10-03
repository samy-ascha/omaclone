//! Byte copy between the two laptops over Thunderbolt.
//!
//! The target opens its disk only after the source serial and size are checked.
//! Sockets are bound to `thunderbolt0`, and a peer that is not link-local is dropped.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::disk::{self, human_bytes};
use crate::model::{App, CONTROLLER_SERIAL, CopyView, Role};

const IFACE: &str = "thunderbolt0";
const PORT: u16 = 39441;
const MAGIC: &[u8; 4] = b"OMA1";
const CHUNK: usize = 8 * 1024 * 1024;
/// Pages kept after the cursor so readahead is not thrown away.
const KEEP_CACHE: u64 = 128 * 1024 * 1024;
/// Dirty data allowed on the target before a write is asked to wait.
const DIRTY_LIMIT: u64 = 512 * 1024 * 1024;
const PIPE: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct End {
    pub role: Role,
    pub serial: String,
    pub bytes: u64,
}

pub struct Job {
    pub role: Role,
    pub name: String,
    pub serial: String,
    pub bytes: u64,
}

pub struct Running {
    pub shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Running {
    pub fn shutdown(mut self) {
        self.shared.request_abort();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shared.request_abort();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Snapshot {
    pub view: CopyView,
    pub note: String,
    pub done: u64,
    pub total: u64,
    pub hash: String,
    pub peer: String,
}

pub struct Shared {
    view: AtomicU8,
    done: AtomicU64,
    total: AtomicU64,
    abort: AtomicBool,
    settled: AtomicBool,
    writes: AtomicBool,
    note: Mutex<String>,
    hash: Mutex<String>,
    peer: Mutex<String>,
    deadline: Mutex<Option<Instant>>,
    chunk: AtomicU64,
}

impl Shared {
    fn new(total: u64) -> Arc<Self> {
        Arc::new(Self {
            view: AtomicU8::new(view_byte(CopyView::Waiting)),
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
            abort: AtomicBool::new(false),
            settled: AtomicBool::new(false),
            writes: AtomicBool::new(false),
            note: Mutex::new("Waiting for the other laptop.".to_string()),
            hash: Mutex::new(String::new()),
            peer: Mutex::new(String::new()),
            deadline: Mutex::new(None),
            chunk: AtomicU64::new(CHUNK as u64),
        })
    }

    pub fn request_abort(&self) {
        self.abort.store(true, Ordering::Release);
        if self.is_running() {
            *lock(&self.note) = "Stopping.".to_string();
        }
    }

    pub fn is_running(&self) -> bool {
        self.snapshot_view().busy()
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            view: self.snapshot_view(),
            note: lock(&self.note).clone(),
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            hash: lock(&self.hash).clone(),
            peer: lock(&self.peer).clone(),
        }
    }

    fn snapshot_view(&self) -> CopyView {
        view_from(self.view.load(Ordering::Acquire))
    }

    fn set_view(&self, view: CopyView) {
        self.view.store(view_byte(view), Ordering::Release);
    }

    fn set_note(&self, note: impl Into<String>) {
        *lock(&self.note) = note.into();
    }

    fn set_peer(&self, peer: &End) {
        *lock(&self.peer) = format!("{}    {}", peer.serial, human_bytes(peer.bytes));
    }

    fn aborted(&self) -> bool {
        self.abort.load(Ordering::Acquire)
    }

    fn past_deadline(&self) -> bool {
        lock(&self.deadline).is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn chunk_size(&self) -> usize {
        self.chunk.load(Ordering::Relaxed).max(1) as usize
    }

    fn finish(&self, result: Result<[u8; 32], SessionError>) {
        if self.settled.swap(true, Ordering::AcqRel) {
            return;
        }
        let done = self.done.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let partial =
            self.writes.load(Ordering::Relaxed) && done > 0 && (total == 0 || done < total);
        match result {
            Ok(hash) => {
                self.set_view(CopyView::Done);
                self.set_note("Copy finished.");
                *lock(&self.hash) = hex(&hash);
            }
            Err(SessionError::Abort) => {
                self.set_view(CopyView::Stopped);
                self.set_note(if partial {
                    "Copy stopped. This disk is incomplete."
                } else {
                    "Copy stopped."
                });
            }
            Err(SessionError::Fail(msg)) => {
                self.set_view(CopyView::Failed);
                if partial {
                    self.set_note(format!("error: {msg} This disk is incomplete."));
                } else {
                    self.set_note(format!("error: {msg}"));
                }
            }
        }
    }
}

struct PanicGuard<'a> {
    shared: &'a Shared,
    settled: bool,
}

impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.shared.finish(Err(SessionError::Fail(
                "the copy stopped unexpectedly.".into(),
            )));
        }
    }
}

pub fn apply(app: &mut App, shared: &Shared) {
    let snap = shared.snapshot();
    app.copy_view = snap.view;
    app.copy_note = snap.note;
    app.copy_done = snap.done;
    app.copy_total = snap.total;
    app.copy_hash = snap.hash;
    app.copy_peer = snap.peer;
}

pub fn start(app: &App) -> Result<Running, String> {
    let role = app.role.ok_or_else(|| "no role selected.".to_string())?;
    let disk = app.inventory.disk.as_ref().map_err(|err| err.clone())?;
    let total = if role == Role::Source { disk.bytes } else { 0 };
    let shared = Shared::new(total);
    shared.writes.store(role == Role::Target, Ordering::Relaxed);
    let job = Job {
        role,
        name: disk.name.clone(),
        serial: disk.serial.clone(),
        bytes: disk.bytes,
    };
    if job.serial == CONTROLLER_SERIAL {
        return Err("this disk is not a clone endpoint.".to_string());
    }
    let bg = Arc::clone(&shared);
    let thread = thread::Builder::new()
        .name("omaclone-copy".to_string())
        .spawn(move || worker(job, bg))
        .map_err(|err| err.to_string())?;
    Ok(Running {
        shared,
        thread: Some(thread),
    })
}

pub struct BenchReport {
    pub seconds: f64,
    pub bytes: u64,
    pub hash: String,
    pub wire_rx: u64,
    pub wire_tx: u64,
}

/// Copy `copy_bytes` over Thunderbolt and return when the hash matches.
/// The target still admits on the real disk size, then writes only the source length.
pub fn run_bench(role: Role, disk: &disk::Disk, copy_bytes: u64) -> Result<BenchReport, String> {
    if disk.serial == CONTROLLER_SERIAL {
        return Err("this disk is not a clone endpoint.".to_string());
    }
    if copy_bytes == 0 || copy_bytes > disk.bytes {
        return Err("the bench length does not fit on this disk.".to_string());
    }
    let job_bytes = if role == Role::Source {
        copy_bytes
    } else {
        disk.bytes
    };
    let total = if role == Role::Source { copy_bytes } else { 0 };
    let shared = Shared::new(total);
    shared.writes.store(role == Role::Target, Ordering::Relaxed);
    let budget = 90 + copy_bytes / 40_000_000;
    *lock(&shared.deadline) = Some(Instant::now() + Duration::from_secs(budget.max(180)));
    let job = Job {
        role,
        name: disk.name.clone(),
        serial: disk.serial.clone(),
        bytes: job_bytes,
    };
    let bg = Arc::clone(&shared);
    let handle = thread::Builder::new()
        .name("omaclone-bench".to_string())
        .spawn(move || worker(job, bg))
        .map_err(|err| err.to_string())?;
    let mut started: Option<Instant> = None;
    let mut rx0 = 0u64;
    let mut tx0 = 0u64;
    let mut last_print = Instant::now();
    loop {
        let snap = shared.snapshot();
        if snap.view == CopyView::Copying && started.is_none() {
            started = Some(Instant::now());
            rx0 = iface_counter("rx_bytes");
            tx0 = iface_counter("tx_bytes");
        }
        if !snap.view.busy() {
            break;
        }
        if last_print.elapsed() >= Duration::from_secs(1) {
            let secs = started.map(|at| at.elapsed().as_secs_f64()).unwrap_or(0.0);
            eprintln!(
                "bench {secs:.1}s  {}  {}",
                snap.note,
                progress_bytes(
                    snap.done,
                    if role == Role::Source {
                        copy_bytes
                    } else {
                        snap.total
                    }
                )
            );
            last_print = Instant::now();
        }
        thread::sleep(Duration::from_millis(200));
    }
    let _ = handle.join();
    let snap = shared.snapshot();
    if snap.view != CopyView::Done {
        return Err(if snap.note.is_empty() {
            "the bench did not finish.".to_string()
        } else {
            snap.note
        });
    }
    let seconds = started
        .map(|at| at.elapsed().as_secs_f64())
        .filter(|secs| *secs > 0.0)
        .ok_or_else(|| "the bench did not copy.".to_string())?;
    Ok(BenchReport {
        seconds,
        bytes: snap.done,
        hash: snap.hash,
        wire_rx: iface_counter("rx_bytes").saturating_sub(rx0),
        wire_tx: iface_counter("tx_bytes").saturating_sub(tx0),
    })
}

fn progress_bytes(done: u64, total: u64) -> String {
    if total == 0 {
        human_bytes(done)
    } else {
        format!(
            "{} / {}  {:.0}%",
            human_bytes(done),
            human_bytes(total),
            (done as f64 / total as f64) * 100.0
        )
    }
}

fn iface_counter(name: &str) -> u64 {
    fs::read_to_string(format!("/sys/class/net/{IFACE}/statistics/{name}"))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn worker(job: Job, shared: Arc<Shared>) {
    let mut guard = PanicGuard {
        shared: &shared,
        settled: false,
    };
    let result = run_job(&job, &shared);
    shared.finish(result);
    guard.settled = true;
}

fn run_job(job: &Job, shared: &Shared) -> Result<[u8; 32], SessionError> {
    if job.serial == CONTROLLER_SERIAL {
        return Err(fail("this disk is not a clone endpoint."));
    }
    let link = disk::read_link();
    if let Some(block) = link.copy_block() {
        return Err(fail(block));
    }
    let local_ip: Ipv4Addr = link
        .addr
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|_| fail("Thunderbolt has no IPv4 address."))?;
    let local = End {
        role: job.role,
        serial: job.serial.clone(),
        bytes: job.bytes,
    };
    let hello = encode_end(&local).map_err(fail)?;
    let udp = udp_on(PORT).map_err(fail)?;
    let listener = if job.role == Role::Target {
        Some(listen_on(local_ip, PORT).map_err(fail)?)
    } else {
        None
    };
    let broadcast = SocketAddr::from((Ipv4Addr::new(169, 254, 255, 255), PORT));
    let mut buf = [0u8; 256];
    loop {
        if shared.aborted() {
            return Err(SessionError::Abort);
        }
        if shared.past_deadline() {
            return Err(fail("timed out."));
        }
        if let Err(err) = udp.send_to(&hello, broadcast) {
            return Err(fail(format!("Thunderbolt is not ready. {err}")));
        }
        loop {
            if shared.aborted() {
                return Err(SessionError::Abort);
            }
            match udp.recv_from(&mut buf) {
                Ok((n, src)) => {
                    if !peer_is_link_local(src.ip()) {
                        continue;
                    }
                    if src.ip() == IpAddr::V4(local_ip) {
                        continue;
                    }
                    let Ok(Frame::Hello(peer)) = decode_frame(&buf[..n]) else {
                        continue;
                    };
                    if peer.serial == local.serial {
                        continue;
                    }
                    admit(&local, &peer).map_err(fail)?;
                    shared.set_peer(&peer);
                    if local.role == Role::Source {
                        let IpAddr::V4(ip) = src.ip() else {
                            continue;
                        };
                        match connect_on(local_ip, SocketAddrV4::new(ip, PORT)) {
                            Ok(mut stream) => {
                                return source_session(&mut stream, &local, shared, || {
                                    open_block(&job.name, false)
                                });
                            }
                            Err(_) => continue,
                        }
                    }
                }
                Err(err) if is_timeout(&err) => break,
                Err(err) => return Err(fail(err.to_string())),
            }
        }
        if let Some(listener) = &listener {
            match listener.accept() {
                Ok((mut stream, peer)) => {
                    if !peer_is_link_local(peer.ip()) {
                        continue;
                    }
                    return target_session(&mut stream, &local, shared, || {
                        open_block(&job.name, true)
                    });
                }
                Err(err) if is_timeout(&err) => {}
                Err(err) => return Err(fail(err.to_string())),
            }
        }
    }
}

fn source_session(
    stream: &mut TcpStream,
    local: &End,
    shared: &Shared,
    open_disk: impl FnOnce() -> io::Result<File>,
) -> Result<[u8; 32], SessionError> {
    prepare_stream(stream)?;
    let hello = encode_end(local).map_err(fail)?;
    write_full(stream, &hello, shared)?;
    let peer = match read_frame(stream, shared)? {
        Frame::Hello(peer) => peer,
        Frame::Error(msg) => return Err(fail(msg)),
    };
    admit(local, &peer).map_err(fail)?;
    shared.set_peer(&peer);
    shared.set_view(CopyView::Copying);
    shared.total.store(local.bytes, Ordering::Relaxed);
    let mut disk = open_disk().map_err(|err| fail(format!("cannot open the disk: {err}")))?;
    let hash = copy_file_to_stream(&mut disk, stream, local.bytes, shared)?;
    let mut remote = [0u8; 32];
    read_exact(stream, &mut remote, shared)?;
    if remote != hash {
        let _ = write_full(stream, &[0], shared);
        return Err(fail("the copy hash does not match."));
    }
    write_full(stream, &[1], shared)?;
    Ok(hash)
}

fn target_session(
    stream: &mut TcpStream,
    local: &End,
    shared: &Shared,
    open_disk: impl FnOnce() -> io::Result<File>,
) -> Result<[u8; 32], SessionError> {
    prepare_stream(stream)?;
    let peer = match read_frame(stream, shared)? {
        Frame::Hello(peer) => peer,
        Frame::Error(msg) => return Err(fail(msg)),
    };
    if let Err(err) = admit(local, &peer) {
        let _ = write_frame(stream, &Frame::Error(err.clone()), shared);
        return Err(fail(err));
    }
    shared.set_peer(&peer);
    let mut disk = match open_disk() {
        Ok(disk) => disk,
        Err(err) => {
            let msg = format!("cannot open the disk: {err}");
            let _ = write_frame(stream, &Frame::Error(msg.clone()), shared);
            return Err(fail(msg));
        }
    };
    let hello = encode_end(local).map_err(fail)?;
    write_full(stream, &hello, shared)?;
    shared.set_view(CopyView::Copying);
    shared.total.store(peer.bytes, Ordering::Relaxed);
    let hash = copy_stream_to_file(stream, &mut disk, peer.bytes, shared)?;
    disk.sync_all().map_err(|err| fail(err.to_string()))?;
    write_full(stream, &hash, shared)?;
    let mut flag = [0u8; 1];
    read_exact(stream, &mut flag, shared)?;
    if flag[0] != 1 {
        return Err(fail("the copy hash does not match."));
    }
    Ok(hash)
}

fn copy_file_to_stream(
    disk: &mut File,
    stream: &mut TcpStream,
    total: u64,
    shared: &Shared,
) -> Result<[u8; 32], SessionError> {
    advise_sequential(disk.as_raw_fd());
    let fd = disk.as_raw_fd();
    let chunk = shared.chunk_size();
    let (data_tx, data_rx) = mpsc::sync_channel(PIPE);
    let (hash_tx, hash_rx) = mpsc::sync_channel(PIPE);
    let (spare_tx, spare_rx) = mpsc::sync_channel(PIPE);
    thread::scope(|scope| {
        scope.spawn(|| read_source(disk, total, chunk, shared, data_tx, spare_rx));
        let hasher = scope.spawn(|| hash_chunks(hash_rx, spare_tx));
        let written = write_source(stream, total, shared, fd, data_rx, hash_tx);
        let hash = hasher.join().expect("hasher");
        written.map(|()| hash)
    })
}

fn copy_stream_to_file(
    stream: &mut TcpStream,
    disk: &mut File,
    total: u64,
    shared: &Shared,
) -> Result<[u8; 32], SessionError> {
    advise_sequential(disk.as_raw_fd());
    let fd = disk.as_raw_fd();
    let chunk = shared.chunk_size();
    let (data_tx, data_rx) = mpsc::sync_channel(PIPE);
    let (hash_tx, hash_rx) = mpsc::sync_channel(PIPE);
    let (spare_tx, spare_rx) = mpsc::sync_channel(PIPE);
    let (done_tx, done_rx) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(move || {
            let result = write_target(disk, total, shared, fd, data_rx, hash_tx);
            let _ = done_tx.send(result);
        });
        let hasher = scope.spawn(|| hash_chunks(hash_rx, spare_tx));
        let read = read_target(stream, total, chunk, shared, data_tx, spare_rx);
        let written = done_rx
            .recv()
            .unwrap_or_else(|_| Err(fail("the copy stopped unexpectedly.")));
        let hash = hasher.join().expect("hasher");
        match (read, written) {
            (Ok(()), Ok(())) => Ok(hash),
            (_, Err(err)) => Err(err),
            (Err(err), Ok(())) => Err(err),
        }
    })
}

fn read_source(
    disk: &mut File,
    total: u64,
    chunk: usize,
    shared: &Shared,
    data_tx: SyncSender<Result<Vec<u8>, SessionError>>,
    spare_rx: Receiver<Vec<u8>>,
) {
    let mut sent = 0u64;
    while sent < total {
        if shared.aborted() {
            let _ = data_tx.send(Err(SessionError::Abort));
            return;
        }
        if shared.past_deadline() {
            let _ = data_tx.send(Err(fail("timed out.")));
            return;
        }
        let want = ((total - sent) as usize).min(chunk);
        let mut buf = take_buf(&spare_rx, want);
        let n = match read_disk(disk, &mut buf[..want]) {
            Ok(n) => n,
            Err(err) => {
                let _ = data_tx.send(Err(err));
                return;
            }
        };
        if n == 0 {
            let _ = data_tx.send(Err(fail("the source disk ended early.")));
            return;
        }
        buf.truncate(n);
        sent += n as u64;
        if data_tx.send(Ok(buf)).is_err() {
            return;
        }
    }
}

fn write_source(
    stream: &mut TcpStream,
    total: u64,
    shared: &Shared,
    fd: RawFd,
    data_rx: Receiver<Result<Vec<u8>, SessionError>>,
    hash_tx: SyncSender<Vec<u8>>,
) -> Result<(), SessionError> {
    let mut sent = 0u64;
    let mut dropped = 0u64;
    while sent < total {
        if shared.aborted() {
            return Err(SessionError::Abort);
        }
        if shared.past_deadline() {
            return Err(fail("timed out."));
        }
        let buf = match data_rx.recv() {
            Ok(chunk) => chunk?,
            Err(_) => {
                return Err(if shared.aborted() {
                    SessionError::Abort
                } else {
                    fail("the source disk ended early.")
                });
            }
        };
        write_full(stream, &buf, shared)?;
        sent += buf.len() as u64;
        shared.done.store(sent, Ordering::Relaxed);
        advise_behind(fd, sent, &mut dropped);
        if hash_tx.send(buf).is_err() {
            return Err(fail("the copy stopped unexpectedly."));
        }
    }
    Ok(())
}

fn read_target(
    stream: &mut TcpStream,
    total: u64,
    chunk: usize,
    shared: &Shared,
    data_tx: SyncSender<Result<Vec<u8>, SessionError>>,
    spare_rx: Receiver<Vec<u8>>,
) -> Result<(), SessionError> {
    let mut got = 0u64;
    while got < total {
        if shared.aborted() {
            let _ = data_tx.send(Err(SessionError::Abort));
            return Err(SessionError::Abort);
        }
        if shared.past_deadline() {
            let _ = data_tx.send(Err(fail("timed out.")));
            return Err(fail("timed out."));
        }
        let want = ((total - got) as usize).min(chunk);
        let mut buf = take_buf(&spare_rx, want);
        read_exact(stream, &mut buf[..want], shared)?;
        buf.truncate(want);
        got += want as u64;
        if data_tx.send(Ok(buf)).is_err() {
            return Err(fail("the copy stopped unexpectedly."));
        }
    }
    Ok(())
}

fn write_target(
    disk: &mut File,
    total: u64,
    shared: &Shared,
    fd: RawFd,
    data_rx: Receiver<Result<Vec<u8>, SessionError>>,
    hash_tx: SyncSender<Vec<u8>>,
) -> Result<(), SessionError> {
    let mut got = 0u64;
    let mut kicked = 0u64;
    let mut waited = 0u64;
    let mut dropped = 0u64;
    while got < total {
        if shared.aborted() {
            let _ = disk.sync_all();
            return Err(SessionError::Abort);
        }
        let buf = match data_rx.recv() {
            Ok(Ok(buf)) => buf,
            Ok(Err(err)) => {
                let _ = disk.sync_all();
                return Err(err);
            }
            Err(_) => {
                let _ = disk.sync_all();
                return Err(if shared.aborted() {
                    SessionError::Abort
                } else {
                    fail("The other laptop closed the connection.")
                });
            }
        };
        if let Err(err) = disk.write_all(&buf) {
            let _ = disk.sync_all();
            return Err(fail(err.to_string()));
        }
        got += buf.len() as u64;
        shared.done.store(got, Ordering::Relaxed);
        pace_writeback(fd, got, &mut kicked, &mut waited);
        advise_behind(fd, got, &mut dropped);
        if hash_tx.send(buf).is_err() {
            return Err(fail("the copy stopped unexpectedly."));
        }
    }
    Ok(())
}

fn read_disk(disk: &mut File, buf: &mut [u8]) -> Result<usize, SessionError> {
    let mut off = 0;
    while off < buf.len() {
        match disk.read(&mut buf[off..]) {
            Ok(0) => break,
            Ok(n) => off += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(fail(err.to_string())),
        }
    }
    Ok(off)
}

/// Refuse the controller disk, a matching pair of serials, the same role, and a short target.
pub fn admit(local: &End, peer: &End) -> Result<(), String> {
    if local.serial == CONTROLLER_SERIAL {
        return Err("this disk is not a clone endpoint.".to_string());
    }
    if peer.serial == CONTROLLER_SERIAL {
        return Err("the other disk is not a clone endpoint.".to_string());
    }
    if local.serial.is_empty() || peer.serial.is_empty() {
        return Err("a disk serial is missing.".to_string());
    }
    if local.bytes == 0 || peer.bytes == 0 {
        return Err("a disk size is missing.".to_string());
    }
    if local.serial == peer.serial {
        return Err("both laptops have the same serial.".to_string());
    }
    if local.role == peer.role {
        return Err(match local.role {
            Role::Source => "the other laptop is also the source.".to_string(),
            Role::Target => "the other laptop is also the target.".to_string(),
        });
    }
    let (source, target) = if local.role == Role::Source {
        (local.bytes, peer.bytes)
    } else {
        (peer.bytes, local.bytes)
    };
    if source > target {
        return Err("the target is smaller than the source.".to_string());
    }
    Ok(())
}

pub fn peer_is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            a == 169 && b == 254 && !((c == 0 && d == 0) || (c == 255 && d == 255))
        }
        IpAddr::V6(ip) => (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

pub fn disk_path(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err("refusing the disk name.".to_string());
    }
    Ok(PathBuf::from(format!("/dev/{name}")))
}

fn open_block(name: &str, write: bool) -> io::Result<File> {
    use std::os::unix::fs::FileTypeExt;
    let path = disk_path(name).map_err(|err| io::Error::other(err))?;
    let meta = fs::metadata(&path)?;
    if !meta.file_type().is_block_device() {
        return Err(io::Error::other("not a block device"));
    }
    let mut options = OpenOptions::new();
    if write {
        options.write(true);
    } else {
        options.read(true);
    }
    options.open(path)
}

enum Frame {
    Hello(End),
    Error(String),
}

fn encode_end(end: &End) -> Result<Vec<u8>, String> {
    if end.serial.len() > 40 || !end.serial.is_ascii() {
        return Err("the serial cannot be sent.".to_string());
    }
    let mut buf = Vec::with_capacity(14 + end.serial.len());
    buf.extend_from_slice(MAGIC);
    buf.push(match end.role {
        Role::Source => 1,
        Role::Target => 2,
    });
    buf.push(end.serial.len() as u8);
    buf.extend_from_slice(end.serial.as_bytes());
    buf.extend_from_slice(&end.bytes.to_be_bytes());
    Ok(buf)
}

fn decode_frame(buf: &[u8]) -> Result<Frame, String> {
    if buf.len() < 6 || &buf[..4] != MAGIC {
        return Err("unexpected data on the link.".to_string());
    }
    let kind = buf[4];
    let len = buf[5] as usize;
    let rest = &buf[6..];
    match kind {
        0 => {
            if rest.len() != len {
                return Err("unexpected data on the link.".to_string());
            }
            let msg = std::str::from_utf8(rest)
                .map_err(|_| "unexpected data on the link.".to_string())?;
            Ok(Frame::Error(msg.to_string()))
        }
        1 | 2 => {
            if rest.len() != len + 8 {
                return Err("unexpected data on the link.".to_string());
            }
            let serial = std::str::from_utf8(&rest[..len])
                .map_err(|_| "unexpected data on the link.".to_string())?
                .to_string();
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&rest[len..]);
            Ok(Frame::Hello(End {
                role: if kind == 1 {
                    Role::Source
                } else {
                    Role::Target
                },
                serial,
                bytes: u64::from_be_bytes(bytes),
            }))
        }
        _ => Err("unexpected data on the link.".to_string()),
    }
}

fn write_frame(stream: &mut TcpStream, frame: &Frame, shared: &Shared) -> Result<(), SessionError> {
    let bytes = match frame {
        Frame::Hello(end) => encode_end(end).map_err(fail)?,
        Frame::Error(msg) => {
            let msg = msg.as_bytes();
            let len = msg.len().min(200);
            let mut buf = Vec::with_capacity(6 + len);
            buf.extend_from_slice(MAGIC);
            buf.push(0);
            buf.push(len as u8);
            buf.extend_from_slice(&msg[..len]);
            buf
        }
    };
    write_full(stream, &bytes, shared)
}

fn read_frame(stream: &mut TcpStream, shared: &Shared) -> Result<Frame, SessionError> {
    let mut head = [0u8; 6];
    read_exact(stream, &mut head, shared)?;
    if &head[..4] != MAGIC {
        return Err(fail("unexpected data on the link."));
    }
    let len = head[5] as usize;
    if len > 200 {
        return Err(fail("unexpected data on the link."));
    }
    let extra = if head[4] == 0 { len } else { len + 8 };
    let mut rest = vec![0u8; extra];
    read_exact(stream, &mut rest, shared)?;
    let mut all = head.to_vec();
    all.extend_from_slice(&rest);
    decode_frame(&all).map_err(fail)
}

fn read_exact(stream: &mut TcpStream, buf: &mut [u8], shared: &Shared) -> Result<(), SessionError> {
    let mut off = 0;
    while off < buf.len() {
        if shared.aborted() {
            return Err(SessionError::Abort);
        }
        if shared.past_deadline() {
            return Err(fail("timed out."));
        }
        match stream.read(&mut buf[off..]) {
            Ok(0) => return Err(fail("The other laptop closed the connection.")),
            Ok(n) => off += n,
            Err(err) if is_timeout(&err) || err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(fail(err.to_string())),
        }
    }
    Ok(())
}

fn write_full(stream: &mut TcpStream, bytes: &[u8], shared: &Shared) -> Result<(), SessionError> {
    let mut off = 0;
    while off < bytes.len() {
        if shared.aborted() {
            return Err(SessionError::Abort);
        }
        if shared.past_deadline() {
            return Err(fail("timed out."));
        }
        match stream.write(&bytes[off..]) {
            Ok(0) => return Err(fail("The other laptop closed the connection.")),
            Ok(n) => off += n,
            Err(err) if is_timeout(&err) || err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(fail(err.to_string())),
        }
    }
    Ok(())
}

fn prepare_stream(stream: &TcpStream) -> Result<(), SessionError> {
    stream
        .set_nodelay(true)
        .map_err(|err| fail(err.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(400)))
        .map_err(|err| fail(err.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| fail(err.to_string()))?;
    Ok(())
}

#[derive(Debug)]
enum SessionError {
    Abort,
    Fail(String),
}

fn fail(msg: impl Into<String>) -> SessionError {
    SessionError::Fail(msg.into())
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(err.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn view_byte(view: CopyView) -> u8 {
    match view {
        CopyView::Idle => 0,
        CopyView::Waiting => 1,
        CopyView::Copying => 2,
        CopyView::Done => 3,
        CopyView::Stopped => 4,
        CopyView::Failed => 5,
    }
}

fn view_from(byte: u8) -> CopyView {
    match byte {
        1 => CopyView::Waiting,
        2 => CopyView::Copying,
        3 => CopyView::Done,
        4 => CopyView::Stopped,
        5 => CopyView::Failed,
        _ => CopyView::Idle,
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

fn advise_sequential(fd: RawFd) {
    unsafe {
        libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_SEQUENTIAL);
    }
}

/// Drop cache only behind the cursor. The live window stays so readahead can run.
fn advise_behind(fd: RawFd, pos: u64, cursor: &mut u64) {
    let Some((start, end)) = lag_range(pos, KEEP_CACHE, *cursor) else {
        return;
    };
    unsafe {
        libc::posix_fadvise(
            fd,
            start as libc::off_t,
            (end - start) as libc::off_t,
            libc::POSIX_FADV_DONTNEED,
        );
    }
    *cursor = end;
}

/// Start writeback, and wait only for data older than `DIRTY_LIMIT`.
fn pace_writeback(fd: RawFd, pos: u64, kicked: &mut u64, waited: &mut u64) {
    if let Some((start, end)) = lag_range(pos, KEEP_CACHE, *kicked) {
        unsafe {
            libc::sync_file_range(
                fd,
                start as libc::off_t,
                (end - start) as libc::off_t,
                libc::SYNC_FILE_RANGE_WRITE,
            );
        }
        *kicked = end;
    }
    if let Some((start, end)) = lag_range(pos, DIRTY_LIMIT, *waited) {
        unsafe {
            libc::sync_file_range(
                fd,
                start as libc::off_t,
                (end - start) as libc::off_t,
                libc::SYNC_FILE_RANGE_WAIT_BEFORE
                    | libc::SYNC_FILE_RANGE_WRITE
                    | libc::SYNC_FILE_RANGE_WAIT_AFTER,
            );
        }
        *waited = end;
    }
}

fn lag_range(pos: u64, keep: u64, cursor: u64) -> Option<(u64, u64)> {
    let end = pos.checked_sub(keep)?;
    if end <= cursor {
        None
    } else {
        Some((cursor, end))
    }
}

fn take_buf(spare: &Receiver<Vec<u8>>, want: usize) -> Vec<u8> {
    let mut buf = spare.try_recv().unwrap_or_default();
    if buf.len() < want {
        buf.resize(want, 0);
    }
    buf
}

fn hash_chunks(chunks: Receiver<Vec<u8>>, spare: SyncSender<Vec<u8>>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    while let Ok(buf) = chunks.recv() {
        hasher.update(&buf);
        // A full or closed spare pool must not stop the hash. The buffer is dropped.
        let _ = spare.try_send(buf);
    }
    *hasher.finalize().as_bytes()
}

pub(crate) fn udp_on(port: u16) -> Result<UdpSocket, String> {
    let fd = Fd::socket(libc::SOCK_DGRAM)?;
    set_int(fd.raw(), libc::SO_REUSEADDR, 1)?;
    set_int(fd.raw(), libc::SO_BROADCAST, 1)?;
    bind_device(fd.raw(), IFACE)?;
    bind_v4(fd.raw(), Ipv4Addr::UNSPECIFIED, port)?;
    let socket = unsafe { UdpSocket::from_raw_fd(fd.into_raw()) };
    socket
        .set_read_timeout(Some(Duration::from_millis(300)))
        .map_err(|err| err.to_string())?;
    Ok(socket)
}

pub(crate) fn listen_on(ip: Ipv4Addr, port: u16) -> Result<TcpListener, String> {
    let fd = Fd::socket(libc::SOCK_STREAM)?;
    set_int(fd.raw(), libc::SO_REUSEADDR, 1)?;
    bind_device(fd.raw(), IFACE)?;
    bind_v4(fd.raw(), ip, port)?;
    let rc = unsafe { libc::listen(fd.raw(), 16) };
    if rc != 0 {
        return Err(format!("listen: {}", io::Error::last_os_error()));
    }
    let listener = unsafe { TcpListener::from_raw_fd(fd.into_raw()) };
    listener
        .set_nonblocking(true)
        .map_err(|err| err.to_string())?;
    Ok(listener)
}

pub(crate) fn connect_on(local: Ipv4Addr, peer: SocketAddrV4) -> Result<TcpStream, String> {
    let fd = Fd::socket(libc::SOCK_STREAM)?;
    bind_device(fd.raw(), IFACE)?;
    bind_v4(fd.raw(), local, 0)?;
    set_timeval(fd.raw(), libc::SO_SNDTIMEO, Duration::from_millis(500))?;
    let mut addr = unsafe { std::mem::zeroed::<libc::sockaddr_in>() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = peer.port().to_be();
    addr.sin_addr.s_addr = u32::from_ne_bytes(peer.ip().octets());
    let rc = unsafe {
        libc::connect(
            fd.raw(),
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!("connect: {}", io::Error::last_os_error()));
    }
    Ok(unsafe { TcpStream::from_raw_fd(fd.into_raw()) })
}

struct Fd(RawFd);

impl Fd {
    fn socket(kind: libc::c_int) -> Result<Self, String> {
        let fd = unsafe { libc::socket(libc::AF_INET, kind | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            Err(format!("socket: {}", io::Error::last_os_error()))
        } else {
            Ok(Self(fd))
        }
    }

    fn raw(&self) -> RawFd {
        self.0
    }

    fn into_raw(mut self) -> RawFd {
        let fd = self.0;
        self.0 = -1;
        fd
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { libc::close(self.0) };
        }
    }
}

fn set_int(fd: RawFd, opt: libc::c_int, value: libc::c_int) -> Result<(), String> {
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            opt,
            &value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("setsockopt: {}", io::Error::last_os_error()))
    }
}

fn set_timeval(fd: RawFd, opt: libc::c_int, dur: Duration) -> Result<(), String> {
    let tv = libc::timeval {
        tv_sec: dur.as_secs() as libc::time_t,
        tv_usec: dur.subsec_micros() as libc::suseconds_t,
    };
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            opt,
            &tv as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("setsockopt: {}", io::Error::last_os_error()))
    }
}

fn bind_device(fd: RawFd, iface: &str) -> Result<(), String> {
    let name = std::ffi::CString::new(iface).map_err(|_| "bad interface name".to_string())?;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            name.as_ptr() as *const libc::c_void,
            name.as_bytes_with_nul().len() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "cannot use {iface}: {}",
            io::Error::last_os_error()
        ))
    }
}

fn bind_v4(fd: RawFd, ip: Ipv4Addr, port: u16) -> Result<(), String> {
    let mut addr = unsafe { std::mem::zeroed::<libc::sockaddr_in>() };
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = port.to_be();
    addr.sin_addr.s_addr = u32::from_ne_bytes(ip.octets());
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("bind: {}", io::Error::last_os_error()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static TEMPS: AtomicU64 = AtomicU64::new(0);

    fn pair(source_bytes: u64, target_bytes: u64) -> (End, End) {
        (
            End {
                role: Role::Source,
                serial: "S29CNYDG898371".to_string(),
                bytes: source_bytes,
            },
            End {
                role: Role::Target,
                serial: "S2Z5NY0H998108".to_string(),
                bytes: target_bytes,
            },
        )
    }

    fn shared_for_test(total: u64) -> Arc<Shared> {
        let shared = Shared::new(total);
        *lock(&shared.deadline) = Some(Instant::now() + Duration::from_secs(3));
        shared.chunk.store(64, Ordering::Relaxed);
        shared
    }

    #[test]
    fn admit_rejects_the_controller_a_short_target_and_the_same_serial() {
        let (source, target) = pair(100, 150);
        assert!(admit(&source, &target).is_ok());
        let mut controller = source.clone();
        controller.serial = CONTROLLER_SERIAL.to_string();
        assert!(
            admit(&controller, &target)
                .unwrap_err()
                .contains("not a clone endpoint")
        );
        assert!(
            admit(&target, &controller)
                .unwrap_err()
                .contains("other disk")
        );
        let mut same = target.clone();
        same.serial = source.serial.clone();
        same.role = Role::Target;
        assert!(admit(&source, &same).unwrap_err().contains("same serial"));
        let mut also = source.clone();
        also.serial = "S2Z5NY0H998100".to_string();
        assert!(
            admit(&source, &also)
                .unwrap_err()
                .contains("also the source")
        );
        let (big, small) = pair(200, 100);
        assert!(admit(&big, &small).unwrap_err().contains("smaller"));
        assert!(admit(&small, &big).unwrap_err().contains("smaller"));
    }

    #[test]
    fn only_link_local_peers_are_accepted() {
        assert!(peer_is_link_local("169.254.251.250".parse().unwrap()));
        assert!(peer_is_link_local("fe80::1".parse().unwrap()));
        assert!(!peer_is_link_local("127.0.0.1".parse().unwrap()));
        assert!(!peer_is_link_local("192.168.2.13".parse().unwrap()));
        assert!(!peer_is_link_local("169.254.255.255".parse().unwrap()));
        assert!(!peer_is_link_local("10.123.123.103".parse().unwrap()));
    }

    #[test]
    fn disk_path_is_only_a_device_name() {
        assert_eq!(disk_path("sda").unwrap(), PathBuf::from("/dev/sda"));
        assert_eq!(disk_path("nvme0n1").unwrap(), PathBuf::from("/dev/nvme0n1"));
        assert!(disk_path("../sda").is_err());
        assert!(disk_path("sda/sdb").is_err());
        assert!(disk_path("").is_err());
    }

    #[test]
    fn hello_round_trips() {
        let (source, _) = pair(251_000_193_024, 251_000_193_024);
        let bytes = encode_end(&source).unwrap();
        match decode_frame(&bytes).unwrap() {
            Frame::Hello(end) => assert_eq!(end, source),
            Frame::Error(msg) => panic!("{msg}"),
        }
    }

    #[test]
    fn a_larger_source_never_opens_the_target_disk() {
        let (source, target) = pair(200, 100);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = shared_for_test(0);
        let opened = Arc::new(AtomicBool::new(false));
        let opened_flag = Arc::clone(&opened);
        let server_shared = Arc::clone(&shared);
        thread::scope(|scope| {
            let server = scope.spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                target_session(&mut stream, &target, &server_shared, || {
                    opened_flag.store(true, Ordering::Relaxed);
                    Err(io::Error::other("should not open"))
                })
            });
            let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            client.write_all(&encode_end(&source).unwrap()).unwrap();
            let mut buf = [0u8; 128];
            let n = client.read(&mut buf).unwrap();
            match decode_frame(&buf[..n]).unwrap() {
                Frame::Error(msg) => assert!(msg.contains("smaller")),
                Frame::Hello(_) => panic!("target accepted a larger source"),
            }
            let err = server.join().unwrap().unwrap_err();
            match err {
                SessionError::Fail(msg) => assert!(msg.contains("smaller")),
                SessionError::Abort => panic!("aborted"),
            }
        });
        assert!(!opened.load(Ordering::Relaxed));
    }

    #[test]
    fn lag_range_keeps_the_live_window() {
        assert_eq!(lag_range(100, 128, 0), None);
        assert_eq!(lag_range(200, 128, 0), Some((0, 72)));
        assert_eq!(lag_range(200, 128, 72), None);
        assert_eq!(lag_range(400, 128, 72), Some((72, 272)));
    }

    #[test]
    fn bytes_and_hash_round_trip_across_chunks() {
        let n = TEMPS.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omaclone-copy-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let payload: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let src_path = dir.join("src");
        let dest_path = dir.join("dest");
        fs::write(&src_path, &payload).unwrap();
        let (source, target) = pair(payload.len() as u64, payload.len() as u64);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = shared_for_test(payload.len() as u64);
        let server_shared = Arc::clone(&shared);
        let client_shared = Arc::clone(&shared);
        thread::scope(|scope| {
            let dest = dest_path.clone();
            let server = scope.spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                target_session(&mut stream, &target, &server_shared, || {
                    OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&dest)
                })
            });
            let src = src_path.clone();
            let client = scope.spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                source_session(&mut stream, &source, &client_shared, || File::open(&src))
            });
            let written = server.join().unwrap().unwrap();
            let read = client.join().unwrap().unwrap();
            assert_eq!(written, read);
            let mut hasher = blake3::Hasher::new();
            hasher.update(&payload);
            let expect: [u8; 32] = *hasher.finalize().as_bytes();
            assert_eq!(written, expect);
        });
        assert_eq!(fs::read(&dest_path).unwrap(), payload);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn abort_writes_nothing() {
        let n = TEMPS.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omaclone-abort-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src_path = dir.join("src");
        let dest_path = dir.join("dest");
        fs::write(&src_path, vec![7u8; 500]).unwrap();
        let (source, target) = pair(500, 500);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = shared_for_test(500);
        let server_shared = Arc::clone(&shared);
        let client_shared = Arc::clone(&shared);
        thread::scope(|scope| {
            let dest = dest_path.clone();
            let server = scope.spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                target_session(&mut stream, &target, &server_shared, move || {
                    OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&dest)
                })
            });
            let src = src_path.clone();
            let abort_shared = Arc::clone(&shared);
            let client = scope.spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                source_session(&mut stream, &source, &client_shared, || {
                    abort_shared.request_abort();
                    File::open(&src)
                })
            });
            let server_result = server.join().unwrap();
            let client_result = client.join().unwrap();
            assert!(client_result.is_err());
            assert!(server_result.is_err());
        });
        let dest = fs::read(&dest_path).unwrap_or_default();
        assert!(dest.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
