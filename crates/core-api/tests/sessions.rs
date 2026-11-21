use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use core_api::{
    FRAME_INTERVAL, MAX_FRAME_BYTES, OutputSink, SessionManager, UNACKED_HIGH_WATERMARK,
};
use core_ssh::{
    AcceptAll, AuthMethod, ConnectParams, ControlMsg, HostKeyVerifier, ShellControl, ShellEnd,
    ShellHandle, SshError, SshTransport,
};
use tokio::sync::{mpsc, oneshot};

/// Same bounded capacity as the real transport's output channel.
const OUTPUT_CAPACITY: usize = 64;

struct FakeTransport {
    handle: Mutex<Option<ShellHandle>>,
}

#[async_trait]
impl SshTransport for FakeTransport {
    async fn open_shell(
        &self,
        _p: ConnectParams,
        _v: Arc<dyn HostKeyVerifier>,
    ) -> Result<ShellHandle, SshError> {
        self.handle.lock().unwrap().take().ok_or(SshError::Closed)
    }
}

#[derive(Default)]
struct RecordingSink {
    frames: Mutex<Vec<Vec<u8>>>,
    closed: Mutex<Vec<Option<String>>>,
    exit_statuses: Mutex<Vec<Option<u32>>>,
}

impl RecordingSink {
    fn frames(&self) -> Vec<Vec<u8>> {
        self.frames.lock().unwrap().clone()
    }
    fn total(&self) -> usize {
        self.frames.lock().unwrap().iter().map(Vec::len).sum()
    }
    fn closed(&self) -> Vec<Option<String>> {
        self.closed.lock().unwrap().clone()
    }
    fn exit_statuses(&self) -> Vec<Option<u32>> {
        self.exit_statuses.lock().unwrap().clone()
    }
}

impl OutputSink for RecordingSink {
    fn frame(&self, bytes: Vec<u8>) {
        self.frames.lock().unwrap().push(bytes);
    }
    fn closed(&self, reason: Option<String>, exit_status: Option<u32>) {
        self.closed.lock().unwrap().push(reason);
        self.exit_statuses.lock().unwrap().push(exit_status);
    }
}

struct Harness {
    mgr: SessionManager,
    id: String,
    out_tx: mpsc::Sender<Bytes>,
    ctl_tx: mpsc::Sender<ControlMsg>,
    ctl_rx: mpsc::Receiver<ControlMsg>,
    sink: Arc<RecordingSink>,
}

fn params() -> ConnectParams {
    ConnectParams {
        host: "fake".into(),
        port: 22,
        username: "u".into(),
        auth: AuthMethod::Password("pw".to_string().into()),
        term: "xterm-256color".into(),
        cols: 80,
        rows: 24,
        connect_timeout: Duration::from_secs(1),
        keepalive: None,
        env: Vec::new(),
    }
}

async fn harness_with_control_capacity(ctl_cap: usize) -> Harness {
    let (out_tx, out_rx) = mpsc::channel(OUTPUT_CAPACITY);
    let (ctl_tx, ctl_rx) = mpsc::channel(ctl_cap);
    let handle = ShellHandle::from_parts(out_rx, ShellControl::from_sender(ctl_tx.clone()));
    let transport = Arc::new(FakeTransport {
        handle: Mutex::new(Some(handle)),
    });
    let mgr = SessionManager::new(transport);
    let sink = Arc::new(RecordingSink::default());
    let id = mgr
        .open_shell(params(), Arc::new(AcceptAll), sink.clone())
        .await
        .unwrap();
    Harness {
        mgr,
        id,
        out_tx,
        ctl_tx,
        ctl_rx,
        sink,
    }
}

async fn harness() -> Harness {
    harness_with_control_capacity(16).await
}

#[tokio::test(start_paused = true)]
async fn coalesces_small_chunks_into_one_frame() {
    let h = harness().await;
    for _ in 0..100 {
        h.out_tx.send(Bytes::from(vec![b'a'; 10])).await.unwrap();
    }
    tokio::time::sleep(FRAME_INTERVAL * 2).await;
    let frames = h.sink.frames();
    assert_eq!(frames.len(), 1, "frames: {}", frames.len());
    assert_eq!(frames[0].len(), 1000);
}

#[tokio::test(start_paused = true)]
async fn splits_at_max_frame_bytes() {
    let h = harness().await;
    let len = 3 * MAX_FRAME_BYTES + 1234;
    h.out_tx.send(Bytes::from(vec![7u8; len])).await.unwrap();
    tokio::time::sleep(FRAME_INTERVAL * 2).await;
    let frames = h.sink.frames();
    // Three full frames cut as the data arrives, then the 1234-byte remainder
    // flushed by the frame timer.
    let lens: Vec<usize> = frames.iter().map(Vec::len).collect();
    assert_eq!(
        lens,
        vec![MAX_FRAME_BYTES, MAX_FRAME_BYTES, MAX_FRAME_BYTES, 1234]
    );
    assert_eq!(frames.concat(), vec![7u8; len]);
}

#[tokio::test(start_paused = true)]
async fn batches_preserve_raw_bytes() {
    let h = harness().await;
    let raw = [0xff, 0xfe, 0x1b, 0x5b, 0x00];
    h.out_tx.send(Bytes::copy_from_slice(&raw)).await.unwrap();
    tokio::time::sleep(FRAME_INTERVAL * 2).await;
    assert_eq!(h.sink.frames().concat(), raw);
}

#[tokio::test(start_paused = true)]
async fn pauses_at_unacked_watermark() {
    let h = harness().await;
    let chunk = Bytes::from(vec![1u8; MAX_FRAME_BYTES / 4]);
    // Enough attempts to cross the watermark and then fill the output channel.
    let attempts = UNACKED_HIGH_WATERMARK / chunk.len() + 2 * OUTPUT_CAPACITY + 16;

    // Producer: push until the channel is full (the manager has stopped reading).
    let mut sent = 0usize;
    let mut full = false;
    for _ in 0..attempts {
        match h.out_tx.try_send(chunk.clone()) {
            Ok(()) => sent += chunk.len(),
            Err(mpsc::error::TrySendError::Full(_)) => {
                full = true;
                // Give the manager a chance to drain if it were still reading.
                tokio::time::sleep(FRAME_INTERVAL * 4).await;
                if matches!(
                    h.out_tx.try_send(chunk.clone()),
                    Err(mpsc::error::TrySendError::Full(_))
                ) {
                    break;
                }
                full = false;
                sent += chunk.len();
            }
            Err(e) => panic!("{e}"),
        }
        tokio::task::yield_now().await;
    }
    assert!(full, "channel never filled: manager did not pause");
    tokio::time::sleep(FRAME_INTERVAL * 4).await;

    let total = h.sink.total();
    assert!(total >= UNACKED_HIGH_WATERMARK, "total {total}");
    assert!(
        total <= UNACKED_HIGH_WATERMARK + MAX_FRAME_BYTES,
        "total {total}"
    );
    assert!(sent > total, "sender should have outrun the manager");

    // Still paused: nothing new arrives without an ack.
    tokio::time::sleep(FRAME_INTERVAL * 10).await;
    assert_eq!(h.sink.total(), total);

    // Ack resumes reading.
    h.mgr.ack(&h.id, total);
    tokio::time::sleep(FRAME_INTERVAL * 4).await;
    assert!(h.sink.total() > total, "reading did not resume after ack");
}

#[tokio::test(start_paused = true)]
async fn closed_reason_propagates() {
    let h = harness().await;
    h.out_tx.send(Bytes::from_static(b"bye")).await.unwrap();
    drop(h.out_tx);
    tokio::time::sleep(FRAME_INTERVAL * 2).await;
    assert_eq!(h.sink.closed(), vec![None]);
    // Pending bytes are flushed before the close notification.
    assert_eq!(h.sink.frames().concat(), b"bye");
    // Session is gone.
    assert!(matches!(
        h.mgr.write(&h.id, b"x".to_vec()).await,
        Err(SshError::Closed)
    ));
}

/// Opens a session on a fake shell that reports how it ended through `end`.
async fn open_with_end(
    end: oneshot::Receiver<ShellEnd>,
) -> (
    SessionManager,
    mpsc::Sender<Bytes>,
    mpsc::Receiver<ControlMsg>,
    Arc<RecordingSink>,
) {
    let (out_tx, out_rx) = mpsc::channel(OUTPUT_CAPACITY);
    let (ctl_tx, ctl_rx) = mpsc::channel(16);
    let handle = ShellHandle::from_parts(out_rx, ShellControl::from_sender(ctl_tx)).with_end(end);
    let mgr = SessionManager::new(Arc::new(FakeTransport {
        handle: Mutex::new(Some(handle)),
    }));
    let sink = Arc::new(RecordingSink::default());
    mgr.open_shell(params(), Arc::new(AcceptAll), sink.clone())
        .await
        .unwrap();
    (mgr, out_tx, ctl_rx, sink)
}

/// A lost connection is `closed(Some(reason))`, never the `None` of a
/// shell that exited; pending output is still delivered first.
#[tokio::test(start_paused = true)]
async fn closed_reason_is_some_on_transport_error() {
    let (end_tx, end_rx) = oneshot::channel();
    let (_mgr, out_tx, _ctl_rx, sink) = open_with_end(end_rx).await;
    out_tx.send(Bytes::from_static(b"last")).await.unwrap();
    end_tx
        .send(ShellEnd::Lost("connection reset by peer".into()))
        .unwrap();
    drop(out_tx);
    tokio::time::sleep(FRAME_INTERVAL * 2).await;
    assert_eq!(sink.frames().concat(), b"last");
    assert_eq!(sink.closed(), vec![Some("connection reset by peer".into())]);
    assert_eq!(sink.exit_statuses(), vec![None]);
}

/// A remote exit is `closed(None)`, with the exit status when the server
/// reported one.
#[tokio::test(start_paused = true)]
async fn remote_exit_is_none_with_exit_status() {
    let (end_tx, end_rx) = oneshot::channel();
    let (_mgr, out_tx, _ctl_rx, sink) = open_with_end(end_rx).await;
    end_tx
        .send(ShellEnd::Exited {
            exit_status: Some(3),
        })
        .unwrap();
    drop(out_tx);
    tokio::time::sleep(FRAME_INTERVAL * 2).await;
    assert_eq!(sink.closed(), vec![None]);
    assert_eq!(sink.exit_statuses(), vec![Some(3)]);
}

#[tokio::test(start_paused = true)]
async fn close_is_bounded_even_when_control_is_full() {
    let h = harness_with_control_capacity(1).await;
    // Fill the control channel and never drain it.
    h.ctl_tx
        .try_send(ControlMsg::Write(Bytes::from_static(b"stuck")))
        .unwrap();

    let start = tokio::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(1), h.mgr.close(&h.id))
        .await
        .expect("close must not block on a full control channel");
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(h.sink.closed(), vec![None]);
    // The output receiver was dropped: the producer sees the channel closed.
    assert!(h.out_tx.send(Bytes::from_static(b"x")).await.is_err());
    drop(h.ctl_rx);
}

#[tokio::test(start_paused = true)]
async fn close_requests_remote_close_and_is_idempotent() {
    let mut h = harness().await;
    h.mgr.close(&h.id).await;
    assert!(matches!(h.ctl_rx.recv().await, Some(ControlMsg::Close)));
    assert_eq!(h.sink.closed(), vec![None]);
    h.mgr.close(&h.id).await; // unknown id: no-op
    assert_eq!(h.sink.closed(), vec![None]);
}

#[tokio::test(start_paused = true)]
async fn write_and_resize_forward_to_control() {
    let mut h = harness().await;
    h.mgr.write(&h.id, b"ls\n".to_vec()).await.unwrap();
    h.mgr.resize(&h.id, 100, 30).await.unwrap();
    assert!(matches!(h.ctl_rx.recv().await, Some(ControlMsg::Write(b)) if &b[..] == b"ls\n"));
    assert!(matches!(
        h.ctl_rx.recv().await,
        Some(ControlMsg::Resize {
            cols: 100,
            rows: 30
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn unknown_session_is_closed_error() {
    let h = harness().await;
    assert!(matches!(
        h.mgr.write("nope", vec![1]).await,
        Err(SshError::Closed)
    ));
    assert!(matches!(
        h.mgr.resize("nope", 80, 24).await,
        Err(SshError::Closed)
    ));
    h.mgr.ack("nope", 10); // must not panic
}

/// Sends `chunk` until the output channel is full (the session has stopped
/// reading), then lets the session settle.
async fn push_until_paused(h: &Harness, chunk: &Bytes) {
    let attempts = UNACKED_HIGH_WATERMARK / chunk.len() + 2 * OUTPUT_CAPACITY + 16;
    for _ in 0..attempts {
        if h.out_tx.try_send(chunk.clone()).is_err() {
            break;
        }
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(FRAME_INTERVAL * 4).await;
}

#[tokio::test(start_paused = true)]
async fn over_ack_saturates_and_does_not_wedge() {
    let h = harness().await;
    // Ack far more than has been delivered: unacked must saturate at 0, not
    // wrap around to a huge value (which would stall the session forever).
    h.mgr.ack(&h.id, usize::MAX);
    h.mgr.ack(&h.id, 10 * UNACKED_HIGH_WATERMARK);

    // Data still flows...
    h.out_tx.send(Bytes::from_static(b"hello")).await.unwrap();
    tokio::time::sleep(FRAME_INTERVAL * 2).await;
    assert_eq!(h.sink.frames().concat(), b"hello");

    // ...and the watermark still bites: the earlier over-ack must not have
    // pre-credited the session.
    let chunk = Bytes::from(vec![1u8; MAX_FRAME_BYTES / 4]);
    push_until_paused(&h, &chunk).await;
    let total = h.sink.total();
    assert!(total >= UNACKED_HIGH_WATERMARK, "total {total}");
    assert!(
        total <= UNACKED_HIGH_WATERMARK + MAX_FRAME_BYTES + 5,
        "total {total}"
    );
    tokio::time::sleep(FRAME_INTERVAL * 10).await;
    assert_eq!(h.sink.total(), total, "must stay paused without an ack");

    // An ack larger than what is outstanding releases the pause.
    h.mgr.ack(&h.id, usize::MAX);
    push_until_paused(&h, &chunk).await;
    assert!(h.sink.total() > total, "did not resume after over-ack");

    // The counter went to 0, not past it: the watermark applies again.
    let resumed = h.sink.total();
    push_until_paused(&h, &chunk).await;
    tokio::time::sleep(FRAME_INTERVAL * 10).await;
    assert!(h.sink.total() <= resumed + UNACKED_HIGH_WATERMARK + MAX_FRAME_BYTES);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_manager_ends_sessions_and_closes_sinks() {
    let h = harness().await;
    h.out_tx.send(Bytes::from_static(b"x")).await.unwrap();
    let Harness {
        mgr, out_tx, sink, ..
    } = h;
    drop(mgr);
    tokio::time::sleep(FRAME_INTERVAL * 4).await;
    assert_eq!(sink.closed(), vec![None], "sink closed exactly once");
    // The session task ended and dropped the output receiver.
    assert!(out_tx.is_closed());
    assert!(out_tx.send(Bytes::from_static(b"y")).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn frame_timer_is_armed_by_data_not_free_running() {
    let h = harness().await;
    // Idle for a while, off the 16 ms grid.
    tokio::time::sleep(Duration::from_millis(10)).await;
    h.out_tx.send(Bytes::from_static(b"a")).await.unwrap();

    // A free-running 16 ms tick (started at t=0) would flush at t=16 ms.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        h.sink.frames().is_empty(),
        "flushed before the frame interval"
    );
    // The timer was armed by the byte at t=10 ms: flush at t=26 ms.
    tokio::time::sleep(Duration::from_millis(8)).await;
    assert_eq!(h.sink.frames().concat(), b"a");

    // After the flush the timer is disarmed: a long idle emits nothing and a
    // later byte waits a full interval again.
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(h.sink.frames().len(), 1);
    h.out_tx.send(Bytes::from_static(b"b")).await.unwrap();
    tokio::time::sleep(FRAME_INTERVAL / 2).await;
    assert_eq!(h.sink.frames().len(), 1);
    tokio::time::sleep(FRAME_INTERVAL).await;
    assert_eq!(h.sink.frames().concat(), b"ab");
}

/// A sink that can be told to panic, to exercise the session task's guard.
#[derive(Default)]
struct PanickySink {
    panic_in_frame: bool,
    panic_in_closed: bool,
    closed: Mutex<Vec<Option<String>>>,
}

impl OutputSink for PanickySink {
    fn frame(&self, _bytes: Vec<u8>) {
        if self.panic_in_frame {
            panic!("sink exploded in frame");
        }
    }
    fn closed(&self, reason: Option<String>, _exit_status: Option<u32>) {
        self.closed.lock().unwrap().push(reason);
        if self.panic_in_closed {
            panic!("sink exploded in closed");
        }
    }
}

/// The control receiver is returned too: while it is alive a write to a
/// still-registered session succeeds, so `Err(Closed)` proves deregistration.
async fn open_with_sink(
    sink: Arc<dyn OutputSink>,
) -> (
    SessionManager,
    String,
    mpsc::Sender<Bytes>,
    mpsc::Receiver<ControlMsg>,
) {
    let (out_tx, out_rx) = mpsc::channel(OUTPUT_CAPACITY);
    let (ctl_tx, ctl_rx) = mpsc::channel(16);
    let handle = ShellHandle::from_parts(out_rx, ShellControl::from_sender(ctl_tx));
    let mgr = SessionManager::new(Arc::new(FakeTransport {
        handle: Mutex::new(Some(handle)),
    }));
    let id = mgr
        .open_shell(params(), Arc::new(AcceptAll), sink)
        .await
        .unwrap();
    (mgr, id, out_tx, ctl_rx)
}

#[tokio::test(start_paused = true)]
async fn panicking_frame_still_deregisters_and_closes_once() {
    let sink = Arc::new(PanickySink {
        panic_in_frame: true,
        ..Default::default()
    });
    let (mgr, id, out_tx, _ctl_rx) = open_with_sink(sink.clone()).await;
    mgr.write(&id, b"alive".to_vec())
        .await
        .expect("registered session accepts writes");
    out_tx.send(Bytes::from_static(b"boom")).await.unwrap();
    tokio::time::sleep(FRAME_INTERVAL * 4).await;

    let closed = sink.closed.lock().unwrap().clone();
    assert_eq!(closed.len(), 1, "closed exactly once: {closed:?}");
    assert!(closed[0].is_some(), "a panic is reported as a reason");
    assert!(matches!(
        mgr.write(&id, b"x".to_vec()).await,
        Err(SshError::Closed)
    ));
    assert!(out_tx.is_closed(), "shell torn down");
    // Closing the dead session is a harmless no-op.
    mgr.close(&id).await;
    assert_eq!(sink.closed.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn panicking_closed_still_deregisters() {
    let sink = Arc::new(PanickySink {
        panic_in_closed: true,
        ..Default::default()
    });
    let (mgr, id, out_tx, _ctl_rx) = open_with_sink(sink.clone()).await;
    mgr.write(&id, b"alive".to_vec())
        .await
        .expect("registered session accepts writes");
    drop(out_tx);
    tokio::time::sleep(FRAME_INTERVAL * 4).await;

    assert_eq!(sink.closed.lock().unwrap().len(), 1, "closed called once");
    assert!(matches!(
        mgr.write(&id, b"x".to_vec()).await,
        Err(SshError::Closed)
    ));
    // The manager is still usable and `close` does not hang on the dead task.
    tokio::time::timeout(Duration::from_secs(1), mgr.close(&id))
        .await
        .expect("close must return");
}
