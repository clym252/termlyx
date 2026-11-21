//! Local shells on a pseudo-terminal, for the desktop app.
//!
//! [`LocalShellTransport`] produces the same [`ShellHandle`] an SSH shell
//! does, so [`SessionManager`](crate::SessionManager) batches, back-pressures
//! and closes local sessions exactly like remote ones.
//!
//! The pty APIs are blocking, so each shell is driven by four plain OS
//! threads. None of them ever waits on another's blocking call, so closing a
//! session is never stuck behind, say, a write the foreground program is not
//! reading:
//!
//! * **reader**: copies pty output into the bounded output channel with
//!   `blocking_send`. A full channel blocks this thread (and so the child,
//!   via the pty buffer): output is back-pressured, never dropped.
//! * **writer**: the only thread that writes to the pty. It drains a queue
//!   filled by the control thread, and blocks whenever the foreground program
//!   is not reading its input.
//! * **control**: applies [`ControlMsg`]s without ever blocking on the pty:
//!   `Write` is queued, `Resize` is an ioctl, and `Close` (or every
//!   [`ShellControl`] being dropped) hangs up the child's process group,
//!   escalating to a kill if it lingers (on Windows: terminates the console
//!   programs in the shell's Job Object, so its whole process tree except
//!   GUI apps it started). The child dying makes the blocked writer fail,
//!   which ends it.
//! * **waiter**: reaps the child. After it exits, and the reader has been
//!   given a moment to drain, it ends the output channel even when a
//!   background job still holds the pty open (so the master never sees EOF).
//!
//! Keystrokes (`Write`) are never logged.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use core_ssh::{ControlMsg, ShellControl, ShellHandle, SshError};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc;

/// Same bounded capacity as the SSH transport's output channel.
const OUTPUT_CAPACITY: usize = 64;
const CONTROL_CAPACITY: usize = 64;
const READ_BUF_BYTES: usize = 32 * 1024;
/// Input the child has not consumed yet is queued up to this much (a single
/// write is always accepted into an empty queue, however large). Beyond it
/// input is discarded and the user is told; see [`InputQueue::offer`].
const MAX_PENDING_INPUT_BYTES: usize = 8 * 1024 * 1024;
/// Shown in the terminal when input had to be discarded.
const INPUT_DISCARDED_NOTICE: &[u8] =
    b"\r\n[termlyx] input discarded: the foreground program is not reading\r\n";
/// After the child exits, how long the reader may sit idle in `read` before
/// the output is considered finished.
const DRAIN_IDLE: Duration = Duration::from_millis(200);
const DRAIN_POLL: Duration = Duration::from_millis(20);
/// Hard cap on the post-exit drain, so a background job that keeps writing to
/// the pty cannot hold the session open forever.
const DRAIN_MAX: Duration = Duration::from_secs(2);
/// On close: how long the child gets to die after SIGHUP before SIGKILL.
const HANGUP_GRACE: Duration = Duration::from_secs(2);

/// Starts shells on a local pseudo-terminal.
pub struct LocalShellTransport;

impl LocalShellTransport {
    /// Spawns `shell` with `args` (or the default shell, see
    /// [`local_shells`]) on a new pty of `cols` x `rows`, in `cwd` (or the inherited directory), with
    /// `TERM=xterm-256color`.
    pub async fn open(
        shell: Option<PathBuf>,
        args: Vec<String>,
        cwd: Option<PathBuf>,
        cols: u16,
        rows: u16,
    ) -> Result<ShellHandle, SshError> {
        tokio::task::spawn_blocking(move || spawn_local(shell, args, cwd, cols, rows))
            .await
            .map_err(|e| SshError::Channel(format!("local shell: {e}")))?
    }
}

fn channel_err(what: &str, e: impl std::fmt::Display) -> SshError {
    SshError::Channel(format!("local shell: {what}: {e}"))
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// A local shell the user can pick. `program` and `args` stay in Rust: the
/// UI only ever sees `id` and `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShell {
    /// Stable across runs (`pwsh`, `cmd`, `wsl:<distro>`, `default`).
    pub id: String,
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[cfg(any(windows, test))]
fn shell(id: &str, label: &str, program: impl Into<PathBuf>, args: &[&str]) -> LocalShell {
    LocalShell {
        id: id.to_owned(),
        label: label.to_owned(),
        program: program.into(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    }
}

/// How long `wsl.exe -l -q` gets before WSL is treated as absent.
#[cfg(windows)]
const WSL_LIST_TIMEOUT: Duration = Duration::from_secs(2);

/// The shells this machine offers, default first. Windows: PowerShell 7 (when
/// `pwsh.exe` is on `PATH`), Windows PowerShell, Command Prompt, then one
/// entry per WSL distro. Elsewhere: the user's `$SHELL`.
pub fn local_shells() -> Vec<LocalShell> {
    #[cfg(windows)]
    {
        windows_shells_from(
            find_on_path("pwsh.exe"),
            &system_root(),
            std::env::var_os("COMSPEC"),
            wsl_list().as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        vec![unix_shell_from(std::env::var_os("SHELL"))]
    }
}

/// The shell with `id` (from [`local_shells`]), or `None` if this machine
/// has no such shell. Never probes WSL, which can take seconds to start:
/// `wsl:<distro>` maps straight to `wsl.exe -d <distro>` (a distro since
/// removed shows wsl's own error in the tab).
pub fn local_shell(id: &str) -> Option<LocalShell> {
    #[cfg(windows)]
    {
        windows_shell_by_id(
            id,
            find_on_path("pwsh.exe"),
            &system_root(),
            std::env::var_os("COMSPEC"),
        )
    }
    #[cfg(not(windows))]
    {
        (id == "default").then(default_shell)
    }
}

/// The shell used when none is chosen: the first of [`local_shells`]. (Not
/// the whole list: probing WSL is not worth a delay on every new tab.)
fn default_shell() -> LocalShell {
    #[cfg(windows)]
    {
        windows_shells_from(
            find_on_path("pwsh.exe"),
            &system_root(),
            std::env::var_os("COMSPEC"),
            None,
        )
        .remove(0)
    }
    #[cfg(not(windows))]
    {
        unix_shell_from(std::env::var_os("SHELL"))
    }
}

/// `$SHELL`, else `/bin/zsh` on macOS and `/bin/bash` elsewhere. A GUI app
/// does not inherit the user's login environment (PATH, ...), so it starts as
/// a login shell, like a terminal emulator's.
#[cfg(any(unix, test))]
fn unix_shell_from(env_shell: Option<OsString>) -> LocalShell {
    #[cfg(target_os = "macos")]
    let fallback = "/bin/zsh";
    #[cfg(not(target_os = "macos"))]
    let fallback = "/bin/bash";
    let program = env_shell
        .filter(|s| !s.is_empty())
        .map_or_else(|| PathBuf::from(fallback), PathBuf::from);
    let label = program
        .file_name()
        .map_or_else(|| "Shell".to_owned(), |n| n.to_string_lossy().into_owned());
    LocalShell {
        id: "default".to_owned(),
        label,
        program,
        args: vec!["-l".to_owned()],
    }
}

/// The Windows list, from already-probed inputs (so it is testable anywhere).
/// `wsl_list` is the raw output of `wsl.exe -l -q`, if it ran.
#[cfg(any(windows, test))]
fn windows_shells_from(
    pwsh: Option<PathBuf>,
    system_root: &std::path::Path,
    comspec: Option<OsString>,
    wsl_list: Option<&[u8]>,
) -> Vec<LocalShell> {
    let system32 = system_root.join("System32");
    let mut shells = Vec::new();
    if let Some(pwsh) = pwsh {
        shells.push(shell("pwsh", "PowerShell", pwsh, &[]));
    }
    shells.push(shell(
        "powershell",
        "Windows PowerShell",
        system32
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
        &[],
    ));
    let cmd = comspec
        .filter(|c| !c.is_empty())
        .map_or_else(|| system32.join("cmd.exe"), PathBuf::from);
    shells.push(shell("cmd", "Command Prompt", cmd, &[]));
    for distro in wsl_list.and_then(parse_wsl_list).unwrap_or_default() {
        shells.extend(wsl_shell(&system32, &distro));
    }
    shells
}

/// `wsl.exe -d <distro>`; `None` for an empty name or one with control
/// characters.
#[cfg(any(windows, test))]
fn wsl_shell(system32: &std::path::Path, distro: &str) -> Option<LocalShell> {
    if distro.is_empty() || distro.chars().any(char::is_control) {
        return None;
    }
    Some(shell(
        &format!("wsl:{distro}"),
        &format!("WSL: {distro}"),
        system32.join("wsl.exe"),
        &["-d", distro],
    ))
}

/// [`local_shell`] on Windows, from already-probed inputs. `default` is the
/// first shell of the list.
#[cfg(any(windows, test))]
fn windows_shell_by_id(
    id: &str,
    pwsh: Option<PathBuf>,
    system_root: &std::path::Path,
    comspec: Option<OsString>,
) -> Option<LocalShell> {
    if let Some(distro) = id.strip_prefix("wsl:") {
        return wsl_shell(&system_root.join("System32"), distro);
    }
    let mut fixed = windows_shells_from(pwsh, system_root, comspec, None);
    if id == "default" {
        return Some(fixed.remove(0));
    }
    fixed.into_iter().find(|s| s.id == id)
}

/// Distro names from `wsl.exe -l -q` output (UTF-16LE, one per line). `None`
/// when it is not text of that shape.
#[cfg(any(windows, test))]
fn parse_wsl_list(bytes: &[u8]) -> Option<Vec<String>> {
    let (pairs, odd) = bytes.as_chunks::<2>();
    if !odd.is_empty() {
        return None;
    }
    let units: Vec<u16> = pairs.iter().map(|p| u16::from_le_bytes(*p)).collect();
    let text = String::from_utf16(&units).ok()?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\r' | '\n'))
    {
        return None;
    }
    Some(
        text.trim_start_matches('\u{feff}')
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// First `name` in a `PATH` directory. `symlink_metadata`, because Store
/// installs are app-execution aliases that `metadata` cannot always follow.
#[cfg(windows)]
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| std::fs::symlink_metadata(p).is_ok())
}

#[cfg(windows)]
fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot")
        .filter(|s| !s.is_empty())
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

/// Raw output of `wsl.exe -l -q`; `None` when WSL is absent, fails or takes
/// longer than [`WSL_LIST_TIMEOUT`].
#[cfg(windows)]
fn wsl_list() -> Option<Vec<u8>> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let wsl = system_root().join("System32").join("wsl.exe");
    if !wsl.is_file() {
        return None;
    }
    let mut child = Command::new(wsl)
        .args(["-l", "-q"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(WSL_LIST_TIMEOUT).ok();
    let ok = out.is_some() && child.wait().is_ok_and(|s| s.success());
    if out.is_none() {
        // Stuck: kill it, which also lets the reader thread finish.
        let _ = child.kill();
        let _ = child.wait();
    }
    out.filter(|_| ok)
}

fn build_command(
    shell: Option<PathBuf>,
    args: Vec<String>,
    cwd: Option<PathBuf>,
) -> CommandBuilder {
    let (program, args) = match shell {
        Some(program) => (program, args),
        None => {
            let d = default_shell();
            (d.program, d.args)
        }
    };
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    cmd.env("TERM", "xterm-256color");
    if let Some(cwd) = cwd {
        cmd.cwd(cwd);
    }
    cmd
}

/// Input waiting for the writer thread.
#[derive(Default)]
struct InputQueue {
    items: VecDeque<Bytes>,
    bytes: usize,
    /// No more input will be written (closed, or the pty write failed).
    closed: bool,
    /// The queue overflowed: everything offered is discarded until the writer
    /// has drained it.
    discarding: bool,
}

/// What [`InputQueue::offer`] did with a write.
#[derive(Debug, PartialEq, Eq)]
enum Offer {
    Queued,
    /// Discarded; this is the first discard of the episode (tell the user).
    DiscardedFirst,
    Discarded,
}

impl InputQueue {
    /// Queue `data` for the writer.
    ///
    /// Input is never delivered with a gap *inside* the queue: a write is
    /// either queued whole or discarded. A write into an empty queue is always
    /// queued, whatever its size (the program may well be reading). Once the
    /// backlog would exceed [`MAX_PENDING_INPUT_BYTES`] the write is discarded
    /// and so is everything after it until the writer has drained the queue;
    /// the caller reports the first discard to the user.
    fn offer(&mut self, data: Bytes) -> Offer {
        if self.items.is_empty() {
            self.discarding = false;
        }
        let first = !self.discarding;
        if !self.items.is_empty()
            && (self.discarding || self.bytes + data.len() > MAX_PENDING_INPUT_BYTES)
        {
            self.discarding = true;
            return if first {
                Offer::DiscardedFirst
            } else {
                Offer::Discarded
            };
        }
        self.bytes += data.len();
        self.items.push_back(data);
        Offer::Queued
    }
}

/// State shared by the four threads of one shell (and a pending discard
/// notice's, see [`report_discarded`]).
struct Shared {
    /// The authoritative output sender. Dropping it (with the reader's
    /// per-chunk clone and a pending notice's) is what closes the output
    /// channel.
    out: Mutex<Option<mpsc::Sender<Bytes>>>,
    input: Mutex<InputQueue>,
    input_cv: Condvar,
    /// The reader is currently blocked in `read`.
    in_read: AtomicBool,
    /// Number of successful reads so far.
    reads: AtomicU64,
    reader_done: AtomicBool,
    /// The child has been reaped (its pid may be recycled: never signal it
    /// afterwards).
    exited: Mutex<bool>,
    exited_cv: Condvar,
    /// A discard notice is waiting for room in the output channel.
    notice_pending: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn new(out: mpsc::Sender<Bytes>) -> Self {
        Self {
            out: Mutex::new(Some(out)),
            input: Mutex::new(InputQueue::default()),
            input_cv: Condvar::new(),
            in_read: AtomicBool::new(false),
            reads: AtomicU64::new(0),
            reader_done: AtomicBool::new(false),
            exited: Mutex::new(false),
            exited_cv: Condvar::new(),
            notice_pending: AtomicBool::new(false),
        }
    }

    fn end_output(&self) {
        lock(&self.out).take();
    }

    fn close_input(&self) {
        lock(&self.input).closed = true;
        self.input_cv.notify_all();
    }
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), SshError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(drop)
        .map_err(|e| channel_err("thread", e))
}

/// A write end for the master. Dropping it must only close it.
///
/// On unix this is a plain `dup` of the master descriptor rather than
/// portable-pty's `take_writer`: that writer, when dropped, writes a newline
/// and EOF to the pty, which would *submit* whatever the user had typed on the
/// line when the session is closed. On Windows `take_writer` hands out the
/// ConPTY input pipe itself, which dropping only closes
/// (`close_does_not_submit_partial_line` holds both to that).
fn pty_writer(master: &(dyn MasterPty + Send)) -> Result<Box<dyn Write + Send>, SshError> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        let raw = master
            .as_raw_fd()
            .ok_or_else(|| channel_err("pty writer", "no master fd"))?;
        // SAFETY: `raw` is the master's open descriptor. F_DUPFD_CLOEXEC
        // returns a new, owned, close-on-exec descriptor (the child must not
        // inherit it).
        let dup = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
        if dup < 0 {
            return Err(channel_err("pty writer", std::io::Error::last_os_error()));
        }
        // SAFETY: `dup` is a fresh descriptor we exclusively own.
        Ok(Box::new(unsafe { std::fs::File::from_raw_fd(dup) }))
    }
    #[cfg(not(unix))]
    master
        .take_writer()
        .map_err(|e| channel_err("pty writer", e))
}

fn spawn_local(
    shell: Option<PathBuf>,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    cols: u16,
    rows: u16,
) -> Result<ShellHandle, SshError> {
    let pair = native_pty_system()
        .openpty(pty_size(cols, rows))
        .map_err(|e| channel_err("open pty", e))?;
    // Take the I/O ends before spawning so a failure here leaves no child.
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| channel_err("pty reader", e))?;
    let writer = pty_writer(&*pair.master)?;
    #[cfg(windows)]
    let job = job::Job::new().map_err(|e| channel_err("job object", e))?;
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut child = pair
        .slave
        .spawn_command(build_command(shell, args, cwd))
        .map_err(|e| channel_err("spawn", e))?;
    // The child holds the only slave end now; keeping ours would stop the
    // reader from ever seeing EOF when the child exits.
    drop(pair.slave);
    // Into the job before it can do much: everything it starts from here on
    // is in the job too. (portable-pty cannot spawn suspended, so a process
    // the shell starts in its first instant would escape.)
    #[cfg(windows)]
    if let Err(e) = child
        .as_raw_handle()
        .ok_or_else(|| std::io::Error::other("no process handle"))
        .and_then(|h| job.assign(h))
    {
        let _ = portable_pty::ChildKiller::kill(&mut *child);
        return Err(channel_err("job object", e));
    }

    let (out_tx, out_rx) = mpsc::channel(OUTPUT_CAPACITY);
    let (ctl_tx, ctl_rx) = mpsc::channel(CONTROL_CAPACITY);
    let shared = Arc::new(Shared::new(out_tx));
    let group = Group {
        #[cfg(unix)]
        pid: child.process_id(),
        #[cfg(windows)]
        job: Arc::new(job),
    };

    let started = (|| {
        let s = shared.clone();
        spawn_thread("tether-pty-write", move || write_loop(&s, writer))?;
        let s = shared.clone();
        spawn_thread("tether-pty-read", move || read_loop(&s, reader))?;
        let s = shared.clone();
        spawn_thread("tether-pty-wait", move || wait_loop(&s, child))?;
        let (s, g) = (shared.clone(), group.clone());
        spawn_thread("tether-pty-ctl", move || {
            control_loop(&s, ctl_rx, pair.master, &g)
        })
    })();
    if let Err(e) = started {
        // Not every thread started: stop the ones that did.
        shared.close_input();
        shared.end_output();
        group.kill();
        return Err(e);
    }
    Ok(ShellHandle::from_parts(
        out_rx,
        ShellControl::from_sender(ctl_tx),
    ))
}

/// The child and everything it started.
///
/// On unix, its process group (portable-pty makes the child a session and
/// group leader, so the pgid is the child's pid). On Windows, a Job Object:
/// descendants inherit it, including programs that get a console of their
/// own and so would survive the pseudoconsole closing. Ending the group ends
/// the console programs in it; GUI apps started from the tab (an editor, a
/// browser) are the user's and keep running. The last clone dropping (the
/// control thread ending) ends whatever console program is left. If Tether
/// itself crashes, nothing ends the job: console programs started with
/// `start` (their own console) keep running, while programs on the tab's
/// pseudoconsole still end when it closes. GUI apps survive a tab close, and
/// consoles started by them end with the tab.
#[derive(Clone)]
struct Group {
    #[cfg(unix)]
    pid: Option<u32>,
    #[cfg(windows)]
    job: Arc<job::Job>,
}

impl Group {
    /// Politely ask the group to go away (SIGHUP, as a closing terminal does).
    /// Windows has no equivalent for a whole tree: the job's console
    /// programs are terminated.
    fn hangup(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: only sends a signal. Callers check the child has not
            // been reaped, so `pid` has not been recycled.
            unsafe {
                libc::killpg(pid, libc::SIGHUP);
            }
        }
        #[cfg(windows)]
        self.job.end_console_programs();
    }

    /// Kill the group outright.
    fn kill(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: as in `hangup`.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        self.job.end_console_programs();
    }
}

/// `IMAGE_SUBSYSTEM_WINDOWS_GUI` in a PE optional header.
#[cfg(any(windows, test))]
const PE_SUBSYSTEM_GUI: u16 = 2;

/// The `Subsystem` field of the PE image read from `r` (an `.exe`), or
/// `None` when it is not a PE image.
#[cfg(any(windows, test))]
fn pe_subsystem(mut r: impl Read + std::io::Seek) -> Option<u16> {
    use std::io::SeekFrom;
    let mut dos = [0u8; 64];
    r.read_exact(&mut dos).ok()?;
    if &dos[..2] != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(dos[60..64].try_into().ok()?);
    r.seek(SeekFrom::Start(u64::from(pe))).ok()?;
    // Signature (4), COFF file header (20), then the optional header, whose
    // `Subsystem` sits at offset 68 in both PE32 and PE32+.
    let mut head = [0u8; 4 + 20 + 70];
    r.read_exact(&mut head).ok()?;
    if &head[..4] != b"PE\0\0" {
        return None;
    }
    let magic = u16::from_le_bytes([head[24], head[25]]);
    if !matches!(magic, 0x10b | 0x20b) {
        return None;
    }
    Some(u16::from_le_bytes([head[24 + 68], head[24 + 69]]))
}

/// Whether closing a tab leaves a program from this image running: only a
/// GUI app does. A console program, or one whose image cannot be read, ends.
#[cfg(any(windows, test))]
fn survives_close(subsystem: Option<u16>) -> bool {
    subsystem == Some(PE_SUBSYSTEM_GUI)
}

#[cfg(windows)]
mod job {
    use std::collections::HashSet;
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, GetLastError, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        JOBOBJECT_BASIC_PROCESS_ID_LIST, JobObjectBasicProcessIdList, QueryInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        QueryFullProcessImageNameW, TerminateProcess,
    };

    /// Passes over the job when ending its console programs: one that a
    /// program starts while the previous pass ends it is caught by the next.
    const END_PASSES: usize = 8;

    /// An owned Job Object handle, closed on drop. The job has no
    /// kill-on-close limit (that would also kill GUI apps started from the
    /// tab): dropping it ends the console programs still in it first.
    pub(super) struct Job(HANDLE);

    // SAFETY: the handle is a kernel object handle, valid on any thread. The
    // job calls made through `&self` are thread-safe; it is closed only in
    // `drop`.
    unsafe impl Send for Job {}
    // SAFETY: as above.
    unsafe impl Sync for Job {}

    impl Job {
        /// A new, unnamed job without limits.
        pub(super) fn new() -> io::Result<Self> {
            // SAFETY: no security attributes (default; the handle is not
            // inheritable, so the shell never holds the job open) and no name.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(handle))
        }

        /// Puts `process` in the job; processes it creates afterwards inherit
        /// the job.
        pub(super) fn assign(&self, process: RawHandle) -> io::Result<()> {
            // SAFETY: `self.0` is open; `process` is a process handle the
            // caller keeps open for the duration of the call.
            if unsafe { AssignProcessToJobObject(self.0, process) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Terminates every console program in the job (exit code 1, as
        /// portable-pty's `kill` uses); GUI apps keep running. Harmless once
        /// they are gone.
        pub(super) fn end_console_programs(&self) {
            let mut ended = HashSet::new();
            for _ in 0..END_PASSES {
                let Ok(pids) = self.pids() else {
                    return;
                };
                let mut found = false;
                for pid in pids {
                    if ended.contains(&pid) {
                        continue;
                    }
                    if let Some(process) = self.console_process(pid) {
                        found = true;
                        ended.insert(pid);
                        // SAFETY: `process` is an open handle with
                        // PROCESS_TERMINATE access, owned for the call.
                        unsafe {
                            TerminateProcess(process.as_raw_handle(), 1);
                        }
                    }
                }
                if !found {
                    return;
                }
            }
        }

        /// The ids of the processes in the job now.
        fn pids(&self) -> io::Result<Vec<u32>> {
            let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
            let mut capacity = 64usize;
            loop {
                let bytes = header + capacity * size_of::<usize>();
                // `usize` words: the list's alignment (its ids are `usize`).
                let mut buf = vec![0usize; bytes.div_ceil(size_of::<usize>())];
                let len = u32::try_from(bytes).map_err(io::Error::other)?;
                // SAFETY: `self.0` is open; `buf` is writable for `len`
                // bytes and aligned for JOBOBJECT_BASIC_PROCESS_ID_LIST, the
                // type this class fills in.
                let ok = unsafe {
                    QueryInformationJobObject(
                        self.0,
                        JobObjectBasicProcessIdList,
                        buf.as_mut_ptr().cast(),
                        len,
                        std::ptr::null_mut(),
                    )
                };
                // SAFETY: read right after the call, on the same thread.
                let more = ok == 0 && unsafe { GetLastError() } == ERROR_MORE_DATA;
                if ok == 0 && !more {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `buf` starts with the list's header, which the
                // call filled in (also on ERROR_MORE_DATA).
                let list = unsafe { &*buf.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
                let assigned = list.NumberOfAssignedProcesses as usize;
                let listed = (list.NumberOfProcessIdsInList as usize).min(capacity);
                if more || listed < assigned {
                    capacity = assigned + 16;
                    continue;
                }
                // SAFETY: `listed` ids follow the header inside `buf` (at
                // most `capacity`, which `buf` holds).
                let ids = unsafe {
                    std::slice::from_raw_parts(
                        buf.as_ptr().cast::<u8>().add(header).cast::<usize>(),
                        listed,
                    )
                };
                return Ok(ids
                    .iter()
                    .filter_map(|&id| u32::try_from(id).ok())
                    .collect());
            }
        }

        /// `pid`, opened for termination, when it is still in this job (not a
        /// recycled id) and a console program.
        fn console_process(&self, pid: u32) -> Option<OwnedHandle> {
            // SAFETY: plain call; a null result means no access or no process.
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                    0,
                    pid,
                )
            };
            if raw.is_null() {
                return None;
            }
            // SAFETY: `raw` is a fresh process handle we exclusively own.
            let process = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut in_job = 0;
            // SAFETY: both handles are open; `in_job` is written by the call.
            let ok = unsafe { IsProcessInJob(process.as_raw_handle(), self.0, &mut in_job) };
            if ok == 0 || in_job == 0 {
                return None;
            }
            let subsystem = image_path(&process)
                .and_then(|path| std::fs::File::open(path).ok())
                .and_then(|file| super::pe_subsystem(io::BufReader::new(file)));
            (!super::survives_close(subsystem)).then_some(process)
        }
    }

    /// The full path of `process`'s executable.
    fn image_path(process: &OwnedHandle) -> Option<PathBuf> {
        let mut buf = vec![0u16; 32 * 1024];
        let mut len = u32::try_from(buf.len()).ok()?;
        // SAFETY: `process` is open with PROCESS_QUERY_LIMITED_INFORMATION;
        // `buf` holds `len` UTF-16 units and the call writes at most that
        // many, then stores the count it wrote in `len`.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                process.as_raw_handle(),
                PROCESS_NAME_WIN32,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        if ok == 0 {
            return None;
        }
        buf.truncate(len as usize);
        Some(OsString::from_wide(&buf).into())
    }

    impl Drop for Job {
        fn drop(&mut self) {
            self.end_console_programs();
            // SAFETY: we own the handle and close it exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn read_loop(shared: &Shared, mut reader: Box<dyn Read + Send>) {
    let mut buf = vec![0u8; READ_BUF_BYTES];
    loop {
        shared.in_read.store(true, Ordering::Release);
        let n = reader.read(&mut buf);
        shared.in_read.store(false, Ordering::Release);
        let n = match n {
            // EOF, or EIO once every slave end is gone.
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        shared.reads.fetch_add(1, Ordering::AcqRel);
        // Clone per chunk and hold no lock while sending (the send blocks
        // under backpressure). The clone is gone before the next `read`.
        let Some(tx) = lock(&shared.out).clone() else {
            break;
        };
        if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
            break; // the session no longer wants output
        }
    }
    shared.reader_done.store(true, Ordering::Release);
    shared.end_output();
}

fn write_loop(shared: &Shared, mut writer: Box<dyn Write + Send>) {
    loop {
        let data = {
            let mut q = lock(&shared.input);
            loop {
                if q.closed {
                    return;
                }
                if let Some(d) = q.items.pop_front() {
                    q.bytes -= d.len();
                    break d;
                }
                q = shared.input_cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        // May block for as long as the foreground program is not reading.
        // Nothing else waits on this thread; the child dying fails the write.
        if writer
            .write_all(&data)
            .and_then(|()| writer.flush())
            .is_err()
        {
            shared.close_input();
            return;
        }
    }
}

/// Blocks until the child has exited, leaving it unreaped. Elsewhere than unix
/// (no zombies, handles instead of pids) this simply waits.
fn wait_without_reaping(child: &(dyn Child + Send + Sync)) {
    #[cfg(unix)]
    if let Some(pid) = child.process_id() {
        loop {
            // SAFETY: `info` is zeroed plain data that waitid fills in; the
            // call only observes our own child (WNOWAIT: no reaping).
            let r = unsafe {
                let mut info: libc::siginfo_t = std::mem::zeroed();
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT,
                )
            };
            if r == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                return;
            }
        }
    }
    let _ = child;
}

fn wait_loop(shared: &Shared, mut child: Box<dyn Child + Send + Sync>) {
    // Observe the exit without reaping, publish it, and only then reap: until
    // `exited` is set the pid is still ours (a zombie at worst), so a signal
    // sent under the `exited` lock can never hit a recycled pid.
    wait_without_reaping(&*child);
    *lock(&shared.exited) = true;
    shared.exited_cv.notify_all();
    let _ = child.wait();
    shared.close_input();

    // The pty only reports EOF once every holder of the slave is gone, and a
    // background job can outlive the shell. Give the reader a moment to drain
    // what the shell wrote, then end the output regardless. "Drained" means
    // it is parked in `read` with nothing new for DRAIN_IDLE; a reader that
    // is busy (or blocked sending to a slow consumer) keeps us waiting, but
    // never longer than DRAIN_MAX.
    let mut last = shared.reads.load(Ordering::Acquire);
    let mut idle = Duration::ZERO;
    let mut waited = Duration::ZERO;
    while !shared.reader_done.load(Ordering::Acquire) && waited < DRAIN_MAX {
        std::thread::sleep(DRAIN_POLL);
        waited += DRAIN_POLL;
        let now = shared.reads.load(Ordering::Acquire);
        if shared.in_read.load(Ordering::Acquire) && now == last {
            idle += DRAIN_POLL;
            if idle >= DRAIN_IDLE {
                break;
            }
        } else {
            idle = Duration::ZERO;
            last = now;
        }
    }
    shared.end_output();
}

fn control_loop(
    shared: &Arc<Shared>,
    mut rx: mpsc::Receiver<ControlMsg>,
    master: Box<dyn MasterPty + Send>,
    group: &Group,
) {
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            ControlMsg::Write(data) => {
                let offer = {
                    let mut q = lock(&shared.input);
                    if q.closed {
                        break; // the pty is gone: end the session
                    }
                    let offer = q.offer(data);
                    shared.input_cv.notify_one();
                    offer
                };
                if offer == Offer::DiscardedFirst {
                    tracing::warn!("local shell input backlog full; discarding input");
                    report_discarded(shared);
                }
            }
            ControlMsg::Resize { cols, rows } => {
                // A failed resize is not fatal to the shell.
                let _ = master.resize(pty_size(cols, rows));
            }
            ControlMsg::Close => break,
        }
    }
    // Refuse further writes at once, then end the child. None of this waits
    // on the writer, which may be blocked for as long as the child lives.
    drop(rx);
    shared.close_input();
    terminate(shared, group);
    // `master` drops here, after the child is gone.
}

/// Tells the user their input is being discarded, without blocking the
/// control thread on the consumer.
///
/// The output channel may well be full when this happens (the tty echoing the
/// flood, a chatty program), and a notice dropped on a full channel is lost
/// exactly when it matters. So if it does not fit at once it is sent from a
/// short-lived thread that waits for room. At most one such thread exists per
/// shell: a stalled consumer cannot pile them up, and one notice per backlog
/// is enough.
fn report_discarded(shared: &Arc<Shared>) {
    let Some(tx) = lock(&shared.out).clone() else {
        return;
    };
    let notice = Bytes::from_static(INPUT_DISCARDED_NOTICE);
    let notice = match tx.try_send(notice) {
        Err(mpsc::error::TrySendError::Full(notice)) => notice,
        _ => return, // sent, or the session no longer wants output
    };
    if shared.notice_pending.swap(true, Ordering::AcqRel) {
        return;
    }
    let s = shared.clone();
    let spawned = spawn_thread("tether-pty-notice", move || {
        let _ = tx.blocking_send(notice);
        s.notice_pending.store(false, Ordering::Release);
    });
    if spawned.is_err() {
        shared.notice_pending.store(false, Ordering::Release);
    }
}

/// Hang up the child's process group; if the child is still alive after
/// [`HANGUP_GRACE`] (it ignores SIGHUP), kill it. Signals are only sent while
/// the child is unreaped, so a recycled pid is never hit. (On Windows the
/// hangup already ends the job's console programs; if the shell had exited
/// first, dropping the job ends the ones it left behind.)
fn terminate(shared: &Shared, group: &Group) {
    let exited = lock(&shared.exited);
    if *exited {
        return;
    }
    group.hangup();
    let (exited, timeout) = shared
        .exited_cv
        .wait_timeout_while(exited, HANGUP_GRACE, |gone| !*gone)
        .unwrap_or_else(|e| e.into_inner());
    if timeout.timed_out() && !*exited {
        group.kill();
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use bytes::Bytes;
    use core_ssh::{AcceptAll, SshError, SshTransport};

    use super::{
        INPUT_DISCARDED_NOTICE, InputQueue, LocalShell, MAX_PENDING_INPUT_BYTES, Offer, Shared,
        local_shell, local_shells, pe_subsystem, report_discarded, survives_close, unix_shell_from,
        windows_shell_by_id, windows_shells_from,
    };

    use crate::{OutputSink, SessionManager, ShellTarget};

    /// Generous: PTY output is real I/O, not simulated time.
    const WAIT: Duration = Duration::from_secs(20);

    #[derive(Default)]
    struct CollectSink {
        out: Mutex<Vec<u8>>,
        closed: Mutex<Vec<Option<String>>>,
    }

    impl CollectSink {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
        }
        fn closed(&self) -> Vec<Option<String>> {
            self.closed.lock().unwrap().clone()
        }
    }

    impl OutputSink for CollectSink {
        fn frame(&self, bytes: Vec<u8>) {
            self.out.lock().unwrap().extend_from_slice(&bytes);
        }
        fn closed(&self, reason: Option<String>, _exit_status: Option<u32>) {
            self.closed.lock().unwrap().push(reason);
        }
    }

    /// The manager's SSH side is never used by local sessions.
    struct NoSsh;

    #[async_trait::async_trait]
    impl SshTransport for NoSsh {
        async fn open_shell(
            &self,
            _p: core_ssh::ConnectParams,
            _v: Arc<dyn core_ssh::HostKeyVerifier>,
        ) -> Result<core_ssh::ShellHandle, SshError> {
            Err(SshError::Closed)
        }
    }

    fn sh() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("cmd.exe")
        } else {
            PathBuf::from("/bin/sh")
        }
    }

    fn local(shell: Option<PathBuf>, cwd: Option<PathBuf>) -> ShellTarget {
        ShellTarget::Local {
            shell,
            args: Vec::new(),
            cwd,
            cols: 80,
            rows: 24,
        }
    }

    async fn open_sh() -> (SessionManager, String, Arc<CollectSink>) {
        let mgr = SessionManager::new(Arc::new(NoSsh));
        let sink = Arc::new(CollectSink::default());
        let id = mgr
            .open(local(Some(sh()), None), Arc::new(AcceptAll), sink.clone())
            .await
            .expect("open local shell");
        answer_cursor_query(&mgr, &id, &sink).await;
        (mgr, id, sink)
    }

    /// Plays the terminal's part in ConPTY's startup handshake. portable-pty
    /// opens the pseudoconsole with `PSEUDOCONSOLE_INHERIT_CURSOR`, so ConPTY
    /// first asks for the cursor position (`ESC[6n`) and holds back all
    /// output, including the shell's, until it gets a report. In the app
    /// xterm.js answers; here nothing would, and every read times out.
    async fn answer_cursor_query(mgr: &SessionManager, id: &str, sink: &CollectSink) {
        if cfg!(windows) {
            wait_for("cursor position query", || sink.text().contains("\x1b[6n")).await;
            mgr.write(id, b"\x1b[1;1R".to_vec()).await.unwrap();
        }
    }

    async fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !ok() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn local_shell_echo() {
        let (mgr, id, sink) = open_sh().await;
        // The tty echoes typed input, so split the word in the command: only
        // the shell's own output contains it whole.
        let cmd: &[u8] = if cfg!(windows) {
            b"echo hi-lo^cal\r\n"
        } else {
            b"echo hi-lo''cal\n"
        };
        mgr.write(&id, cmd.to_vec()).await.unwrap();
        wait_for("echo output", || sink.text().contains("hi-local")).await;
        mgr.close(&id).await;
    }

    #[tokio::test]
    async fn local_resize_ok() {
        let (mgr, id, sink) = open_sh().await;
        mgr.resize(&id, 100, 30).await.unwrap();
        if cfg!(unix) {
            mgr.write(&id, b"stty size\n".to_vec()).await.unwrap();
            wait_for("resized pty", || sink.text().contains("30 100")).await;
        }
        mgr.close(&id).await;
    }

    #[tokio::test]
    async fn exit_closes_sink() {
        let (mgr, id, sink) = open_sh().await;
        let cmd: &[u8] = if cfg!(windows) {
            b"exit\r\n"
        } else {
            b"exit\n"
        };
        mgr.write(&id, cmd.to_vec()).await.unwrap();
        wait_for("closed", || !sink.closed().is_empty()).await;
        assert_eq!(sink.closed(), vec![None]);
        assert!(matches!(
            mgr.write(&id, b"x".to_vec()).await,
            Err(SshError::Closed)
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_sets_term() {
        let (mgr, id, sink) = open_sh().await;
        mgr.write(&id, b"echo T=$TERM\n".to_vec()).await.unwrap();
        wait_for("TERM", || sink.text().contains("T=xterm-256color")).await;
        mgr.close(&id).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_cwd_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let canon = dir.path().canonicalize().unwrap();
        let mgr = SessionManager::new(Arc::new(NoSsh));
        let sink = Arc::new(CollectSink::default());
        let id = mgr
            .open(
                local(Some(sh()), Some(canon.clone())),
                Arc::new(AcceptAll),
                sink.clone(),
            )
            .await
            .unwrap();
        mgr.write(&id, b"echo CWD=$(pwd -P)\n".to_vec())
            .await
            .unwrap();
        let want = format!("CWD={}", canon.display());
        wait_for("cwd", || sink.text().contains(&want)).await;
        mgr.close(&id).await;
    }

    #[tokio::test]
    async fn local_bad_shell_is_an_error() {
        let mgr = SessionManager::new(Arc::new(NoSsh));
        let sink = Arc::new(CollectSink::default());
        let r = mgr
            .open(
                local(Some(PathBuf::from("/definitely/not/a/shell")), None),
                Arc::new(AcceptAll),
                sink.clone(),
            )
            .await;
        assert!(matches!(r, Err(SshError::Channel(_))), "{:?}", r.err());
        assert!(sink.closed().is_empty());
    }

    /// Closing a session must not leave the shell running.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_close_kills_the_child() {
        let (mgr, id, sink) = open_sh().await;
        mgr.write(&id, b"echo PID=$$\n".to_vec()).await.unwrap();
        let pid = {
            let mut found = None;
            wait_for("pid", || {
                let t = sink.text();
                found = t.match_indices("PID=").find_map(|(i, _)| {
                    let digits: String = t[i + 4..]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    (!digits.is_empty()).then_some(digits)
                });
                found.is_some()
            })
            .await;
            found.unwrap()
        };
        mgr.close(&id).await;
        wait_for("child gone", || {
            !std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        })
        .await;
        assert_eq!(sink.closed(), vec![None]);
    }

    #[cfg(unix)]
    async fn wait_for_pid(sink: &CollectSink) -> String {
        let mut found = None;
        wait_for("pid", || {
            let t = sink.text();
            found = t.match_indices("PID=").find_map(|(i, _)| {
                let digits: String = t[i + 4..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                (!digits.is_empty()).then_some(digits)
            });
            found.is_some()
        })
        .await;
        found.unwrap()
    }

    #[cfg(unix)]
    fn pid_alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// Runs `setup` (which must print `PID=<shell pid>` and leave a foreground
    /// program that never reads its input), queues ~64 KB of input that the
    /// pty cannot take, closes the session and asserts the shell is gone
    /// within `limit`. A close that queued behind the blocked write would leave
    /// the shell alive for the foreground program's whole lifetime (30 s).
    #[cfg(unix)]
    async fn close_with_blocked_write_kills_shell(setup: &[u8], limit: Duration) {
        let (mgr, id, sink) = open_sh().await;
        mgr.write(&id, setup.to_vec()).await.unwrap();
        let pid = wait_for_pid(&sink).await;
        // Let the foreground program start, then flood its input.
        tokio::time::sleep(Duration::from_millis(300)).await;
        // Complete lines: the tty keeps them queued for the program, so once
        // its input buffer is full the pty write blocks.
        let lines = "echo flood-flood-flood-flood-flood-flood-flood-flood-flood\n".repeat(1200);
        assert!(lines.len() > 64 * 1024);
        mgr.write(&id, lines.into_bytes()).await.unwrap();
        // Give the writer time to be stuck on a full pty buffer.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let start = tokio::time::Instant::now();
        mgr.close(&id).await;
        wait_for("shell gone", || !pid_alive(&pid)).await;
        assert!(
            start.elapsed() < limit,
            "shell survived close for {:?}",
            start.elapsed()
        );
        assert_eq!(sink.closed(), vec![None]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_close_does_not_wait_on_a_blocked_write() {
        close_with_blocked_write_kills_shell(b"echo PID=$$; sleep 30\n", Duration::from_secs(5))
            .await;
    }

    /// A foreground program that ignores SIGHUP is still killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_close_escalates_when_hangup_is_ignored() {
        close_with_blocked_write_kills_shell(
            b"echo PID=$$; trap '' HUP; sleep 30\n",
            Duration::from_secs(10),
        )
        .await;
    }

    /// Closing a tab must never execute a command the user typed but did not
    /// enter. (portable-pty's own writer injects `\n` + EOF when dropped, so a
    /// writer dropped while the child is alive would submit the line.) The
    /// shell ignores SIGHUP, so it lives until the SIGKILL escalation: the
    /// marker must not appear even then.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_close_never_submits_an_unentered_line() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let (mgr, id, sink) = open_sh().await;
        mgr.write(&id, b"trap '' HUP; echo REA''DY\n".to_vec())
            .await
            .unwrap();
        wait_for("trap installed", || sink.text().contains("READY")).await;
        let typed = format!("touch '{}'", marker.display());
        mgr.write(&id, typed.into_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;

        mgr.close(&id).await;
        // Past the SIGHUP grace period, when the shell is finally killed.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!marker.exists(), "closing submitted the unentered line");
    }

    /// Closing a tab with a half-typed line must not run it, on any platform
    /// (portable-pty's unix writer writes `\n` + EOF when dropped).
    #[tokio::test]
    async fn close_does_not_submit_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let (mgr, id, sink) = open_sh().await;
        let (ready, typed): (&[u8], String) = if cfg!(windows) {
            (
                b"echo REA^DY\r\n",
                format!("echo SUBMITTED> \"{}\"", marker.display()),
            )
        } else {
            (
                b"echo REA''DY\n",
                format!("echo SUBMITTED > '{}'", marker.display()),
            )
        };
        // The shell is at its prompt, reading a line.
        mgr.write(&id, ready.to_vec()).await.unwrap();
        wait_for("shell ready", || sink.text().contains("READY")).await;
        mgr.write(&id, typed.into_bytes()).await.unwrap();
        wait_for("typed line echoed", || sink.text().contains("SUBMITTED")).await;

        mgr.close(&id).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!marker.exists(), "closing submitted the unentered line");
    }

    /// The pids of every running `image` (e.g. `ping.exe`).
    #[cfg(windows)]
    fn pids_of(image: &str) -> std::collections::HashSet<u32> {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {image}"), "/FO", "CSV", "/NH"])
            .output()
            .unwrap();
        // `"ping.exe","1234","Console","1","4,321 K"`; no match prints an
        // INFO line instead.
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split("\",\"").nth(1)?.parse().ok())
            .collect()
    }

    #[cfg(windows)]
    fn ping_pids() -> std::collections::HashSet<u32> {
        pids_of("ping.exe")
    }

    #[cfg(windows)]
    fn taskkill(pid: u32) {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }

    /// Closing a tab ends everything the shell started: a foreground program
    /// on the pseudoconsole and one with a console of its own (`start`),
    /// which closing the pseudoconsole alone would leave running.
    #[cfg(windows)]
    #[tokio::test]
    async fn close_kills_child_processes() {
        let before = ping_pids();
        let (mgr, id, sink) = open_sh().await;
        mgr.write(
            &id,
            b"start \"\" ping -t 127.0.0.1 & ping -t 127.0.0.1\r\n".to_vec(),
        )
        .await
        .unwrap();
        wait_for("ping output", || sink.text().contains("Reply from")).await;
        let mut ours = Vec::new();
        wait_for("both pings running", || {
            ours = ping_pids().difference(&before).copied().collect();
            ours.len() >= 2
        })
        .await;

        mgr.close(&id).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut left: Vec<u32> = ours.clone();
        while !left.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let now = ping_pids();
            left.retain(|p| now.contains(p));
        }
        for pid in &left {
            // Do not leak them past a failed run.
            taskkill(*pid);
        }
        assert!(left.is_empty(), "still running after close: {left:?}");
    }

    /// A GUI program started from a tab is the user's: it keeps running when
    /// the tab closes, while console programs (here a `start`ed ping with a
    /// console of its own) still end.
    #[cfg(windows)]
    #[tokio::test]
    async fn close_leaves_gui_programs_running() {
        let gui_before = pids_of("charmap.exe");
        let ping_before = ping_pids();
        let (mgr, id, _sink) = open_sh().await;
        mgr.write(
            &id,
            b"start \"\" charmap.exe & start \"\" ping -t 127.0.0.1\r\n".to_vec(),
        )
        .await
        .unwrap();
        let (mut gui, mut pings) = (Vec::new(), Vec::new());
        wait_for("charmap and ping running", || {
            gui = pids_of("charmap.exe")
                .difference(&gui_before)
                .copied()
                .collect();
            pings = ping_pids().difference(&ping_before).copied().collect();
            !gui.is_empty() && !pings.is_empty()
        })
        .await;

        mgr.close(&id).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !pings.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let now = ping_pids();
            pings.retain(|p| now.contains(p));
        }
        // Past the close: the job's handle is gone too.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let gui_now = pids_of("charmap.exe");
        let gui_alive: Vec<u32> = gui
            .iter()
            .copied()
            .filter(|p| gui_now.contains(p))
            .collect();
        for pid in gui.iter().chain(&pings) {
            // Never leave them running past the test.
            taskkill(*pid);
        }
        assert!(pings.is_empty(), "console program still running: {pings:?}");
        assert_eq!(gui_alive, gui, "the GUI program was ended by the close");
    }

    /// A background job holds the pty open after the shell exits, so the
    /// master never sees EOF; the session must still end.
    #[cfg(unix)]
    #[tokio::test]
    async fn exit_with_background_job_closes_sink() {
        let (mgr, id, sink) = open_sh().await;
        mgr.write(&id, b"sleep 5 &\n".to_vec()).await.unwrap();
        mgr.write(&id, b"echo done-lo''ng; exit\n".to_vec())
            .await
            .unwrap();
        wait_for("closed", || !sink.closed().is_empty()).await;
        assert_eq!(sink.closed(), vec![None]);
        // Output written before the exit was delivered first.
        assert!(sink.text().contains("done-long"), "{}", sink.text());
        assert!(matches!(
            mgr.write(&id, b"x".to_vec()).await,
            Err(SshError::Closed)
        ));
    }

    /// `Close` ends the shell and its reader thread: the output channel closes
    /// and later writes fail.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_control_close_ends_output() {
        use crate::LocalShellTransport;
        let mut h = LocalShellTransport::open(Some(sh()), Vec::new(), None, 80, 24)
            .await
            .unwrap();
        h.control.close().await;
        let end =
            tokio::time::timeout(WAIT, async { while h.output.recv().await.is_some() {} }).await;
        assert!(end.is_ok(), "output never closed after Close");
        assert!(
            h.control
                .write(bytes::Bytes::from_static(b"x"))
                .await
                .is_err()
        );
    }

    /// Dropping every control handle (what the manager does on teardown) also
    /// ends the shell.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_dropping_control_ends_output() {
        use crate::LocalShellTransport;
        let h = LocalShellTransport::open(Some(sh()), Vec::new(), None, 80, 24)
            .await
            .unwrap();
        let mut output = h.output;
        drop(h.control);
        let end =
            tokio::time::timeout(WAIT, async { while output.recv().await.is_some() {} }).await;
        assert!(end.is_ok(), "output never closed after control dropped");
    }

    fn big(n: usize) -> Bytes {
        Bytes::from(vec![b'x'; n])
    }

    #[test]
    fn input_queue_accepts_a_single_oversized_write_when_empty() {
        let mut q = InputQueue::default();
        assert_eq!(q.offer(big(MAX_PENDING_INPUT_BYTES * 2)), Offer::Queued);
        assert_eq!(q.items.len(), 1);
    }

    #[test]
    fn input_queue_never_leaves_holes_after_overflow() {
        let mut q = InputQueue::default();
        assert_eq!(q.offer(big(MAX_PENDING_INPUT_BYTES - 10)), Offer::Queued);
        // Does not fit: discarded, and the user is told once.
        assert_eq!(q.offer(big(100)), Offer::DiscardedFirst);
        // A smaller write that would fit must NOT be accepted behind the hole.
        assert_eq!(q.offer(big(1)), Offer::Discarded);
        assert_eq!(q.items.len(), 1);
        // Once the writer has drained the queue, input flows again.
        q.items.clear();
        q.bytes = 0;
        assert_eq!(q.offer(big(1)), Offer::Queued);
        // ...and a later overflow is reported afresh.
        assert_eq!(q.offer(big(MAX_PENDING_INPUT_BYTES)), Offer::DiscardedFirst);
    }

    /// The notice is not lost when the output channel is full (the tty echoing
    /// a flood fills it): it arrives once the consumer makes room, and only
    /// one is ever pending.
    #[tokio::test]
    async fn discard_notice_waits_for_room_in_a_full_output_channel() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(Bytes::from_static(b"echo")).unwrap();
        let shared = Arc::new(Shared::new(tx));
        report_discarded(&shared);
        report_discarded(&shared);
        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"echo"));
        assert_eq!(rx.recv().await.unwrap(), INPUT_DISCARDED_NOTICE);
        shared.end_output();
        // The second report found the first still pending: nothing more.
        assert_eq!(rx.recv().await, None);
    }

    /// With a program that is not reading, a flood beyond the backlog cap is
    /// discarded, the user sees a notice, and the session stays usable.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_input_overflow_is_reported_in_the_terminal() {
        let (mgr, id, sink) = open_sh().await;
        mgr.write(&id, b"echo PID=$$; sleep 30\n".to_vec())
            .await
            .unwrap();
        let pid = wait_for_pid(&sink).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let chunk = "echo flood\n".repeat(MAX_PENDING_INPUT_BYTES / 11 / 2);
        for _ in 0..6 {
            mgr.write(&id, chunk.clone().into_bytes()).await.unwrap();
        }
        wait_for("overflow notice", || {
            sink.text().contains("input discarded")
        })
        .await;
        mgr.close(&id).await;
        wait_for("shell gone", || !pid_alive(&pid)).await;
    }

    /// A background job that keeps writing must not hold the session open.
    #[cfg(unix)]
    #[tokio::test]
    async fn exit_with_chatty_background_job_closes_sink() {
        let (mgr, id, sink) = open_sh().await;
        // Bounded (4 s), so no orphan outlives the test run.
        mgr.write(
            &id,
            b"(i=0; while [ $i -lt 40 ]; do echo tick; sleep 0.1; i=$((i+1)); done) &\n".to_vec(),
        )
        .await
        .unwrap();
        mgr.write(&id, b"exit\n".to_vec()).await.unwrap();
        wait_for("closed", || !sink.closed().is_empty()).await;
        assert_eq!(sink.closed(), vec![None]);
    }

    /// A minimal PE image header: DOS stub pointing at `pe`, then the PE
    /// signature, a COFF header and an optional header with `magic` and
    /// `subsystem`.
    fn pe_image(pe: u32, magic: u16, subsystem: u16) -> Vec<u8> {
        let mut img = vec![0u8; pe as usize + 4 + 20 + 240];
        img[..2].copy_from_slice(b"MZ");
        img[60..64].copy_from_slice(&pe.to_le_bytes());
        let p = pe as usize;
        img[p..p + 4].copy_from_slice(b"PE\0\0");
        img[p + 24..p + 26].copy_from_slice(&magic.to_le_bytes());
        img[p + 24 + 68..p + 24 + 70].copy_from_slice(&subsystem.to_le_bytes());
        img
    }

    #[test]
    fn pe_subsystem_reads_gui_and_console_images() {
        let read = |img: Vec<u8>| pe_subsystem(std::io::Cursor::new(img));
        // PE32+ and PE32 GUI, PE32+ console.
        assert_eq!(read(pe_image(0x80, 0x20b, 2)), Some(2));
        assert_eq!(read(pe_image(0xf8, 0x10b, 2)), Some(2));
        assert_eq!(read(pe_image(0x100, 0x20b, 3)), Some(3));
        // Not an image, a bad signature, a truncated header.
        assert_eq!(read(b"#!/bin/sh\n".to_vec()), None);
        let mut bad = pe_image(0x80, 0x20b, 2);
        bad[0x80] = b'X';
        assert_eq!(read(bad), None);
        let mut short = pe_image(0x80, 0x20b, 2);
        short.truncate(0x80 + 30);
        assert_eq!(read(short), None);
    }

    #[test]
    fn only_gui_programs_survive_a_closed_tab() {
        assert!(survives_close(Some(2)));
        assert!(!survives_close(Some(3)), "console");
        assert!(!survives_close(None), "unreadable image");
    }

    fn ids(shells: &[LocalShell]) -> Vec<&str> {
        shells.iter().map(|s| s.id.as_str()).collect()
    }

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    const ROOT: &str = r"C:\Windows";

    #[test]
    fn windows_shells_order_with_pwsh() {
        let shells = windows_shells_from(
            Some(PathBuf::from(r"C:\pwsh\pwsh.exe")),
            Path::new(ROOT),
            Some(r"C:\Windows\System32\cmd.exe".into()),
            None,
        );
        assert_eq!(ids(&shells), ["pwsh", "powershell", "cmd"]);
        assert_eq!(shells[0].label, "PowerShell");
        assert_eq!(shells[0].program, PathBuf::from(r"C:\pwsh\pwsh.exe"));
        assert_eq!(shells[1].label, "Windows PowerShell");
        assert_eq!(
            shells[1].program,
            Path::new(ROOT)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        );
        assert_eq!(shells[2].label, "Command Prompt");
        assert!(shells.iter().all(|s| s.args.is_empty()));
    }

    #[test]
    fn windows_shells_without_pwsh() {
        let shells = windows_shells_from(None, Path::new(ROOT), None, None);
        assert_eq!(ids(&shells), ["powershell", "cmd"]);
    }

    #[test]
    fn windows_cmd_prefers_comspec_then_system32() {
        let with = windows_shells_from(None, Path::new(ROOT), Some(r"D:\x\cmd.exe".into()), None);
        assert_eq!(with[1].program, PathBuf::from(r"D:\x\cmd.exe"));
        for unset in [None, Some(OsString::new())] {
            let without = windows_shells_from(None, Path::new(ROOT), unset, None);
            assert_eq!(
                without[1].program,
                Path::new(ROOT).join("System32").join("cmd.exe")
            );
        }
    }

    #[test]
    fn windows_shells_list_wsl_distros() {
        let list = utf16le("\u{feff}Ubuntu\r\n\r\nDebian\r\n");
        let shells = windows_shells_from(None, Path::new(ROOT), None, Some(&list));
        assert_eq!(
            ids(&shells),
            ["powershell", "cmd", "wsl:Ubuntu", "wsl:Debian"]
        );
        assert_eq!(shells[2].label, "WSL: Ubuntu");
        assert_eq!(
            shells[2].program,
            Path::new(ROOT).join("System32").join("wsl.exe")
        );
        assert_eq!(shells[2].args, ["-d", "Ubuntu"]);
        assert_eq!(shells[3].args, ["-d", "Debian"]);
    }

    #[test]
    fn windows_shells_ignore_unreadable_wsl_output() {
        let garbage: [&[u8]; 3] = [
            &[0xff, 0xfe, 0xfd],       // odd length
            &[0x00, 0xd8, 0x41, 0x00], // lone surrogate
            &[0x01, 0x00, 0x02, 0x00], // control characters
        ];
        for bytes in garbage {
            let shells = windows_shells_from(None, Path::new(ROOT), None, Some(bytes));
            assert_eq!(ids(&shells), ["powershell", "cmd"], "{bytes:?}");
        }
        let empty = windows_shells_from(None, Path::new(ROOT), None, Some(&[]));
        assert_eq!(ids(&empty), ["powershell", "cmd"]);
    }

    /// A chosen shell resolves by id without probing WSL: the fixed ones as
    /// the list has them, `wsl:<distro>` straight to `wsl.exe -d <distro>`.
    #[test]
    fn windows_shell_by_id_needs_no_wsl_probe() {
        let by_id = |id: &str| {
            windows_shell_by_id(
                id,
                Some(PathBuf::from(r"C:\pwsh\pwsh.exe")),
                Path::new(ROOT),
                None,
            )
        };
        let list = windows_shells_from(
            Some(PathBuf::from(r"C:\pwsh\pwsh.exe")),
            Path::new(ROOT),
            None,
            None,
        );
        for id in ["pwsh", "powershell", "cmd"] {
            assert_eq!(by_id(id).as_ref(), list.iter().find(|s| s.id == id), "{id}");
        }
        assert_eq!(by_id("default"), Some(list[0].clone()));
        let wsl = by_id("wsl:Ubuntu-24.04").unwrap();
        assert_eq!(wsl.id, "wsl:Ubuntu-24.04");
        assert_eq!(wsl.label, "WSL: Ubuntu-24.04");
        assert_eq!(
            wsl.program,
            Path::new(ROOT).join("System32").join("wsl.exe")
        );
        assert_eq!(wsl.args, ["-d", "Ubuntu-24.04"]);
        for bad in ["wsl:", "wsl:a\nb", "wsl:\u{7}", "bash", "", "WSL:Ubuntu"] {
            assert_eq!(by_id(bad), None, "{bad:?}");
        }
        let no_pwsh = windows_shell_by_id("pwsh", None, Path::new(ROOT), None);
        assert_eq!(no_pwsh, None);
    }

    #[cfg(unix)]
    #[test]
    fn unix_shell_by_id_is_the_default_only() {
        assert_eq!(local_shell("default"), Some(local_shells().remove(0)));
        assert_eq!(local_shell("cmd"), None);
        assert_eq!(local_shell("wsl:Ubuntu"), None);
    }

    #[test]
    fn unix_shell_is_the_login_shell() {
        let s = unix_shell_from(Some("/usr/bin/fish".into()));
        assert_eq!(s.id, "default");
        assert_eq!(s.label, "fish");
        assert_eq!(s.program, PathBuf::from("/usr/bin/fish"));
        assert_eq!(s.args, ["-l"]);
        let fallback = unix_shell_from(Some("".into()));
        assert_eq!(fallback.program, unix_shell_from(None).program);
        assert!(!fallback.label.is_empty());
    }

    #[test]
    fn local_shells_has_a_default_first() {
        let shells = local_shells();
        assert!(!shells.is_empty());
        assert!(
            shells
                .iter()
                .all(|s| !s.id.is_empty() && !s.label.is_empty())
        );
        let mut seen = std::collections::HashSet::new();
        assert!(
            shells.iter().all(|s| seen.insert(s.id.clone())),
            "ids are unique"
        );
    }

    /// What the Windows dev VM (or any Windows host) offers: PowerShell and
    /// cmd always exist there. Prints the list for the Task 5 report.
    #[cfg(windows)]
    #[test]
    fn windows_host_offers_powershell_and_cmd() {
        let shells = local_shells();
        eprintln!("local_shells: {shells:#?}");
        let ids: Vec<_> = shells.iter().map(|s| s.id.as_str()).collect();
        assert!(
            ids.contains(&"powershell") && ids.contains(&"cmd"),
            "{ids:?}"
        );
        for s in shells
            .iter()
            .filter(|s| ["powershell", "cmd"].contains(&s.id.as_str()))
        {
            assert!(s.program.is_file(), "{:?}", s.program);
        }
        // By id (what opening a tab does), the same shells without the list.
        for id in ["powershell", "cmd"] {
            assert_eq!(
                local_shell(id).as_ref(),
                shells.iter().find(|s| s.id == id),
                "{id}"
            );
        }
        assert_eq!(local_shell("default"), Some(shells[0].clone()));
    }

    /// Extra arguments reach the shell.
    #[tokio::test]
    async fn local_shell_args_are_passed() {
        let mgr = SessionManager::new(Arc::new(NoSsh));
        let sink = Arc::new(CollectSink::default());
        let args: Vec<String> = if cfg!(windows) {
            vec!["/c".into(), "echo ARGS_OK".into()]
        } else {
            vec!["-c".into(), "echo ARGS_OK".into()]
        };
        let id = mgr
            .open(
                ShellTarget::Local {
                    shell: Some(sh()),
                    args,
                    cwd: None,
                    cols: 80,
                    rows: 24,
                },
                Arc::new(AcceptAll),
                sink.clone(),
            )
            .await
            .expect("open local shell");
        answer_cursor_query(&mgr, &id, &sink).await;
        wait_for("args output", || sink.text().contains("ARGS_OK")).await;
        mgr.close(&id).await;
    }
}
