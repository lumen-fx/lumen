//! A surface session against a producer over the Unix transport, with
//! no GPU: the consumer's backend records what it would import and show, and
//! the producer is the protocol crate's own. Everything about the session
//! (handshake, refusals, pacing, slot reuse, resize generations, stalls,
//! input, crashes and reconnects) runs here on any Unix machine.

#![cfg(unix)]

use lumen_surface::protocol::producer::{FrameTimes, Producer, ProducerEvent, ProducerHello};
use lumen_surface::protocol::wire::{
    AdapterId, Api, BUFFER_DEDICATED, BufferDesc, Buffers, ByeReason, Format, InputEvent,
    RejectReason,
};
use lumen_surface::session::{self, Backend, Happened, Imported, Shared, SurfaceStateKind, Wanted};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ADAPTER: AdapterId = AdapterId {
    device_uuid: [7; 16],
    driver_uuid: [8; 16],
    luid: [0; 8],
    registry_id: 0,
    vendor: 0x1002,
    device: 0x73bf,
};

/// A backend that imports nothing and shows nothing, but records both, and
/// lets go of a shown frame when the next one is shown, as a renderer whose
/// GPU work is done would.
#[derive(Default)]
struct Recorder {
    shown: Mutex<Vec<(u32, u32, u64)>>,
    showing: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    imported: Mutex<Vec<(u32, usize)>>,
    generation: AtomicU64,
    /// How long a ring's first-use work takes, as a slow GPU's would.
    prepare: Mutex<Duration>,
}

impl Backend for Recorder {
    type Texture = (u32, u32);

    fn device(&self) -> Option<(Api, AdapterId)> {
        Some((Api::Vulkan, ADAPTER))
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn import(&self, buffers: &Buffers, fds: Vec<OwnedFd>) -> Result<Imported<(u32, u32)>, String> {
        assert_eq!(fds.len(), buffers.buffers.len(), "a descriptor per buffer");
        self.imported
            .lock()
            .unwrap()
            .push((buffers.generation, fds.len()));
        let textures = (0..buffers.buffers.len() as u32)
            .map(|slot| (buffers.generation, slot))
            .collect();
        let prepare = *self.prepare.lock().unwrap();
        if prepare.is_zero() {
            return Ok(Imported::now(textures));
        }
        let until = Instant::now() + prepare;
        Ok(Imported {
            textures,
            ready: Box::new(move || Instant::now() >= until),
        })
    }

    fn present(&self, texture: (u32, u32), stamp: u64, released: Box<dyn FnOnce() + Send>) {
        self.shown
            .lock()
            .unwrap()
            .push((texture.0, texture.1, stamp));
        let previous = self.showing.lock().unwrap().replace(released);
        if let Some(previous) = previous {
            previous();
        }
    }
}

fn hello() -> ProducerHello {
    ProducerHello {
        api: Api::Vulkan,
        adapter: ADAPTER,
        engine: "test producer".into(),
        caps: 0,
    }
}

/// Wait for `check`, up to five seconds.
fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Everything that happened, gathered until `check` holds.
fn wait_happened(
    shared: &Shared,
    seen: &mut Vec<Happened>,
    mut check: impl FnMut(&[Happened]) -> bool,
) {
    wait_for("an event", || {
        seen.extend(shared.take().0);
        check(seen)
    });
}

/// A ring of `n` buffers whose descriptors are open files: the consumer
/// side here imports nothing, it only needs a descriptor per buffer.
fn ring(generation: u32, size: (u32, u32), n: usize) -> (Buffers, Vec<std::fs::File>) {
    let files: Vec<std::fs::File> = (0..n)
        .map(|_| std::fs::File::open("/dev/null").unwrap())
        .collect();
    (
        Buffers {
            generation,
            width: size.0,
            height: size.1,
            format: Format::Rgba8Srgb,
            usage: 0x17,
            flags: BUFFER_DEDICATED,
            buffers: (0..n as u64)
                .map(|handle| BufferDesc {
                    alloc_size: 4096,
                    handle,
                    ..BufferDesc::default()
                })
                .collect(),
        },
        files,
    )
}

fn offer(producer: &mut Producer, generation: u32, size: (u32, u32), n: usize) {
    let (buffers, files) = ring(generation, size, n);
    let fds: Vec<i32> = files.iter().map(|f| f.as_raw_fd()).collect();
    producer
        .offer_buffers(buffers, &fds)
        .expect("the ring is accepted");
}

/// Render one frame: wait for a slot, announce it, and let the consumer's
/// element take it the way its systems do each tick (which asks for the
/// next one).
fn one_frame(producer: &mut Producer, shared: &Shared) -> u64 {
    let before = shared.frame().and_then(|f| f.received_at);
    let slot = producer
        .next_slot(Duration::from_secs(5))
        .expect("the session is up")
        .expect("a slot is free and asked for");
    let frame = producer
        .frame_ready(slot, b"meta", FrameTimes::default())
        .expect("announced");
    wait_for("the frame to show", || {
        shared
            .frame()
            .is_some_and(|f| f.frame == frame && f.received_at != before)
    });
    shared.request_frame();
    frame
}

fn start() -> (Arc<Shared>, Arc<Recorder>) {
    let backend = Arc::new(Recorder::default());
    let shared = session::start("scene", "scene", backend.clone()).expect("listening");
    (shared, backend)
}

/// A producer that knows the endpoint is welcomed at the size the element
/// wants; its frames show one after another and every slot comes back, so a
/// ring of three keeps going for as long as frames are asked for.
#[test]
fn frames_flow_and_slots_come_back() {
    let (shared, backend) = start();
    shared.set_wanted(Wanted {
        width: 320,
        height: 200,
        scale: 1.0,
    });
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    assert_eq!(
        (producer.welcome().width, producer.welcome().height),
        (320, 200)
    );
    offer(&mut producer, 1, (320, 200), 3);
    for _ in 0..10 {
        one_frame(&mut producer, &shared);
    }
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    let shown = backend.shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 10);
    assert!(shown.iter().all(|(g, ..)| *g == 1));
    assert_eq!(shared.frame().unwrap().meta, b"meta");
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| {
        h.iter()
            .any(|e| matches!(e, Happened::Connected { engine, .. } if engine == "test producer"))
    });
    producer.close(ByeReason::Exit, "done");
    wait_happened(&shared, &mut seen, |h| {
        h.iter()
            .any(|e| matches!(e, Happened::Disconnected { reason: "exit", .. }))
    });
    assert_eq!(
        shared.state(),
        SurfaceStateKind::Lost,
        "the last frame stays"
    );
    shared.close();
}

/// A producer with the wrong token, another GPU, or a second one on a busy
/// channel is refused, and told why.
#[test]
fn the_wrong_producer_is_refused() {
    let (shared, _) = start();
    let endpoint = shared.endpoint().expect("a transport");
    let mut forged: lumen_surface::protocol::endpoint::Endpoint = endpoint.parse().unwrap();
    forged.token[0] ^= 1;
    let reason =
        |result: Result<Producer, lumen_surface::protocol::producer::ProducerError>| match result {
            Err(lumen_surface::protocol::producer::ProducerError::Rejected(r)) => r.reason,
            Err(e) => panic!("refused for another reason: {e}"),
            Ok(p) => panic!("welcomed: {:?}", p.welcome()),
        };
    assert_eq!(
        reason(Producer::connect(&forged.to_string(), &hello())),
        RejectReason::Token
    );
    let mut other_gpu = hello();
    other_gpu.adapter.device_uuid = [9; 16];
    assert_eq!(
        reason(Producer::connect(&endpoint, &other_gpu)),
        RejectReason::Adapter
    );
    let first = Producer::connect(&endpoint, &hello()).expect("the first one is welcome");
    assert_eq!(
        reason(Producer::connect(&endpoint, &hello())),
        RejectReason::Busy
    );
    drop(first);
    shared.close();
}

/// A new size goes to the producer, whose next ring is a new generation;
/// the old ring is retired once its slots are back.
#[test]
fn a_resize_brings_a_new_generation_and_retires_the_old() {
    let (shared, backend) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    one_frame(&mut producer, &shared);
    assert!(shared.set_wanted(Wanted {
        width: 640,
        height: 480,
        scale: 2.0,
    }));
    let mut resized = None;
    wait_for("the resize", || {
        while let Ok(Some(event)) = producer.poll_event() {
            if let ProducerEvent::Resize {
                width,
                height,
                scale,
            } = event
            {
                resized = Some((width, height, scale));
            }
        }
        resized.is_some()
    });
    assert_eq!(resized, Some((640, 480, 2.0)));
    offer(&mut producer, 2, (640, 480), 3);
    one_frame(&mut producer, &shared);
    one_frame(&mut producer, &shared);
    assert_eq!(shared.frame().unwrap().size, (640, 480));
    let mut retired = false;
    wait_for("generation 1 to retire", || {
        while let Ok(Some(event)) = producer.poll_event() {
            retired |= event == ProducerEvent::Retired(1);
        }
        retired
    });
    assert_eq!(
        backend.imported.lock().unwrap().clone(),
        vec![(1, 3), (2, 3)]
    );
    shared.close();
}

/// A frame asked for and not delivered within the stall timeout stalls the
/// surface; the next frame resumes it.
#[test]
fn a_late_frame_stalls_and_the_next_resumes() {
    let (shared, _) = start();
    shared.set_stall_timeout(Duration::from_millis(100));
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| {
        h.iter().any(|e| matches!(e, Happened::Stalled { .. }))
    });
    assert_eq!(shared.state(), SurfaceStateKind::Stalled);
    one_frame(&mut producer, &shared);
    wait_happened(&shared, &mut seen, |h| h.contains(&Happened::Resumed));
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    shared.close();
}

/// Input goes to the producer in order with rising sequence numbers, which
/// its frames echo; messages go both ways.
#[test]
fn input_and_messages_cross() {
    let (shared, _) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    let key = InputEvent::Key {
        code: 26,
        pressed: true,
        repeat: false,
        modifiers: 0,
        key: "w".into(),
    };
    let first = shared
        .send_input(InputEvent::Focus(true))
        .expect("connected");
    let second = shared.send_input(key.clone()).expect("connected");
    assert!(second > first);
    assert!(shared.send_user(b"from the app"));
    let mut got = Vec::new();
    wait_for("the input", || {
        while let Ok(Some(event)) = producer.poll_event() {
            got.push(event);
        }
        got.len() >= 3
    });
    assert!(
        matches!(&got[0], ProducerEvent::Input { seq, event: InputEvent::Focus(true), .. } if *seq == first)
    );
    assert!(
        matches!(&got[1], ProducerEvent::Input { seq, event, .. } if *seq == second && *event == key)
    );
    assert_eq!(got[2], ProducerEvent::User(b"from the app".to_vec()));
    let frame = one_frame(&mut producer, &shared);
    assert!(frame > 0);
    assert_eq!(
        shared.frame().unwrap().input_seq,
        second,
        "the frame echoes the input"
    );
    producer.send_user(b"from the game").unwrap();
    producer
        .set_cursor(lumen_surface::protocol::wire::cursor::CROSSHAIR)
        .unwrap();
    producer.request_pointer_lock(true).unwrap();
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| h.len() >= 4);
    assert!(seen.contains(&Happened::User(b"from the game".to_vec())));
    assert!(seen.contains(&Happened::Cursor("crosshair")));
    assert!(seen.contains(&Happened::PointerLock(true)));
    shared.close();
}

/// A producer that dies without a goodbye leaves the surface lost with its
/// last frame; a new producer on the same channel is welcomed and shows
/// frames again.
#[test]
fn a_crash_is_reported_and_a_new_producer_takes_over() {
    let (shared, backend) = start();
    let endpoint = shared.endpoint().expect("a transport");
    let mut producer = Producer::connect(&endpoint, &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    one_frame(&mut producer, &shared);
    drop(producer);
    let mut seen = Vec::new();
    wait_happened(&shared, &mut seen, |h| {
        h.iter().any(|e| {
            matches!(
                e,
                Happened::Disconnected {
                    reason: "crash",
                    ..
                }
            )
        })
    });
    assert_eq!(shared.state(), SurfaceStateKind::Lost);
    assert!(shared.frame().is_some(), "the last frame stays");

    let mut again = Producer::connect(&endpoint, &hello()).expect("welcomed again");
    offer(&mut again, 1, (300, 150), 2);
    one_frame(&mut again, &shared);
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    assert_eq!(backend.shown.lock().unwrap().len(), 2);
    shared.close();
}

/// Nobody seeing the surface is news to the producer; so is the device
/// changing, after which the producer offers a new ring on the same
/// connection.
#[test]
fn visibility_and_device_changes_reach_the_producer() {
    let (shared, backend) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 3);
    one_frame(&mut producer, &shared);
    shared.set_visible(false);
    backend.generation.fetch_add(1, Ordering::SeqCst);
    let mut got = Vec::new();
    wait_for("visibility and the reset", || {
        while let Ok(Some(event)) = producer.poll_event() {
            got.push(event);
        }
        got.contains(&ProducerEvent::Reset)
    });
    assert!(got.contains(&ProducerEvent::Visibility(false)));
    shared.set_visible(true);
    offer(&mut producer, 2, (300, 150), 3);
    one_frame(&mut producer, &shared);
    assert_eq!(shared.frame().unwrap().generation, 2);
    shared.close();
}

/// A producer that stops answering is hung: after a ping goes unanswered the
/// session ends, and the surface listens for the next one.
#[test]
fn a_producer_that_stops_answering_is_hung() {
    let (shared, _) = start();
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    offer(&mut producer, 1, (300, 150), 2);
    // From here the producer never reads its socket.
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen
        .iter()
        .any(|e| matches!(e, Happened::Disconnected { reason: "hung", .. }))
    {
        assert!(
            Instant::now() < deadline,
            "the hung producer was not dropped"
        );
        seen.extend(shared.take().0);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(shared.state(), SurfaceStateKind::Lost);
    drop(producer);
    shared.close();
}

/// A ring whose first-use work outlasts the producer's answer timeout is
/// still accepted: the consumer keeps serving while it prepares, pinging the
/// waiting producer, and every message restarts the producer's wait. The
/// first frame then flows as usual.
#[test]
fn a_ring_that_takes_long_to_prepare_is_still_accepted() {
    let (shared, backend) = start();
    *backend.prepare.lock().unwrap() =
        lumen_surface::protocol::producer::ANSWER_TIMEOUT + Duration::from_secs(1);
    let mut producer =
        Producer::connect(&shared.endpoint().expect("a transport"), &hello()).expect("welcomed");
    let started = Instant::now();
    offer(&mut producer, 1, (64, 48), 3);
    assert!(
        started.elapsed() > lumen_surface::protocol::producer::ANSWER_TIMEOUT,
        "the ring was accepted before its preparation finished"
    );
    shared.request_frame();
    one_frame(&mut producer, &shared);
    assert_eq!(shared.state(), SurfaceStateKind::Live);
    producer.close(ByeReason::Exit, "done");
    shared.close();
}

/// The C ABI a producer in C or C++ links, driven in-process against a real
/// session: every call the reference producer makes, the events it reads
/// back, and the arguments it refuses.
mod c_abi {
    use super::*;
    use lumen_surface::protocol::ffi::*;
    use lumen_surface::protocol::wire::PointerInput;
    use std::ffi::{CStr, CString};

    fn last_error() -> String {
        let mut buf = [0 as std::os::raw::c_char; 256];
        // SAFETY: a writable buffer of the stated length.
        unsafe { lsp_last_error(buf.as_mut_ptr(), buf.len()) };
        // SAFETY: lsp_last_error NUL-terminates.
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    fn hello(engine: &CStr) -> LspHello {
        LspHello {
            struct_size: std::mem::size_of::<LspHello>() as u32,
            api: 1,
            reserved0: 0,
            caps: 0,
            reserved1: 0,
            device_uuid: ADAPTER.device_uuid,
            driver_uuid: ADAPTER.driver_uuid,
            luid: [0; 8],
            registry_id: 0,
            vendor: ADAPTER.vendor,
            device: ADAPTER.device,
            engine: engine.as_ptr(),
        }
    }

    fn welcome() -> LspWelcome {
        LspWelcome {
            struct_size: 0,
            pacing: 0,
            ring_min: 0,
            ring_max: 0,
            ring_pref: 0,
            reserved0: [0; 3],
            width: 0,
            height: 0,
            scale: 0.0,
            formats: 0,
            reserved1: 0,
            session: 0,
        }
    }

    fn event() -> LspEvent {
        // SAFETY: LspEvent is plain data; all-zero is a valid value (null
        // pointers included).
        unsafe { std::mem::zeroed() }
    }

    /// Poll until an event of `kind` arrives, returning it.
    fn poll_for(p: *mut LspProducer, kind: u32) -> LspEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut e = event();
            // SAFETY: a live producer and a writable event.
            assert_eq!(unsafe { lsp_poll(p, &mut e) }, LSP_OK);
            if e.kind == kind {
                return e;
            }
            assert!(Instant::now() < deadline, "no event of kind {kind}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pointer(x: f32) -> PointerInput {
        PointerInput {
            x,
            y: 2.0,
            dx: 1.0,
            dy: -1.0,
            buttons: 1,
            pointer_id: 1,
            pointer_type: 0,
            button: 0,
            pressure: 0.5,
            tilt_x: 0.0,
            tilt_y: 0.0,
            twist: 0.0,
        }
    }

    #[test]
    fn a_c_producer_runs_a_whole_session() {
        assert_eq!(lsp_version() >> 16, 1, "the major version");
        // SAFETY: a static string the library owns.
        let name = unsafe { CStr::from_ptr(lsp_key_code_name(0)) };
        assert_eq!(name.to_str(), Ok("Unidentified"));
        // SAFETY: as above; past the table the name is "Unidentified".
        let past = unsafe { CStr::from_ptr(lsp_key_code_name(u16::MAX)) };
        assert_eq!(past.to_str(), Ok("Unidentified"));

        let (shared, backend) = start();
        shared.set_wanted(Wanted {
            width: 64,
            height: 48,
            scale: 1.0,
        });
        let engine = CString::new("c test producer").unwrap();
        let mut status = LSP_OK;
        let mut w = welcome();

        // Refusals: no endpoint, an endpoint that does not parse, and the
        // wrong token.
        // SAFETY: the null endpoint is the case under test.
        let none = unsafe { lsp_connect(std::ptr::null(), &hello(&engine), &mut w, &mut status) };
        assert!(none.is_null());
        assert_eq!(status, LSP_ERR_ARG);
        assert!(!last_error().is_empty());
        let bad = CString::new("v9;nonsense").unwrap();
        // SAFETY: valid arguments; the endpoint is refused.
        let none = unsafe { lsp_connect(bad.as_ptr(), &hello(&engine), &mut w, &mut status) };
        assert!(none.is_null());
        assert_eq!(status, LSP_ERR_ENDPOINT);
        let endpoint = shared.endpoint().expect("a transport");
        let wrong: lumen_surface::protocol::endpoint::Endpoint = endpoint.parse().unwrap();
        let wrong = lumen_surface::protocol::endpoint::Endpoint {
            token: [0; 16],
            ..wrong
        };
        let wrong = CString::new(wrong.to_string()).unwrap();
        // SAFETY: valid arguments; the token is refused.
        let none = unsafe { lsp_connect(wrong.as_ptr(), &hello(&engine), &mut w, &mut status) };
        assert!(none.is_null());
        assert_eq!(status, LSP_ERR_REJECTED);
        assert!(last_error().contains("token"), "{}", last_error());

        // The adapter the endpoint names, once the consumer knows it.
        let (mut device, mut driver) = ([0u8; 16], [0u8; 16]);
        let plain = CString::new(endpoint.clone()).unwrap();
        // SAFETY: 16 writable bytes each.
        let st = unsafe {
            lsp_endpoint_adapter(plain.as_ptr(), device.as_mut_ptr(), driver.as_mut_ptr())
        };
        assert_eq!(st, LSP_ERR_ARG, "no GPU named yet");
        shared.set_adapter_hint(Some((
            lumen_surface::protocol::endpoint::AdapterHint::Vulkan {
                device_uuid: ADAPTER.device_uuid,
                driver_uuid: ADAPTER.driver_uuid,
            },
            "test GPU".into(),
        )));
        let named = CString::new(shared.endpoint().unwrap()).unwrap();
        // SAFETY: as above.
        let st = unsafe {
            lsp_endpoint_adapter(named.as_ptr(), device.as_mut_ptr(), driver.as_mut_ptr())
        };
        assert_eq!(st, LSP_OK);
        assert_eq!((device, driver), (ADAPTER.device_uuid, ADAPTER.driver_uuid));

        // SAFETY: valid arguments.
        let p = unsafe { lsp_connect(named.as_ptr(), &hello(&engine), &mut w, &mut status) };
        assert!(!p.is_null(), "{}", last_error());
        assert_eq!((w.width, w.height), (64, 48));

        // A ring of three, the descriptors open files.
        let (buffers, files) = ring(1, (64, 48), 3);
        let descs: Vec<LspBuffer> = buffers
            .buffers
            .iter()
            .zip(&files)
            .map(|(b, f)| LspBuffer {
                alloc_size: b.alloc_size,
                memory_type: 0,
                offset: 0,
                row_pitch: 0,
                reserved0: 0,
                modifier: 0,
                fd: f.as_raw_fd(),
                reserved1: 0,
            })
            .collect();
        let offer = LspBuffers {
            struct_size: std::mem::size_of::<LspBuffers>() as u32,
            generation: 1,
            width: 64,
            height: 48,
            format: LSP_FORMAT_RGBA8_SRGB,
            usage: buffers.usage,
            flags: LSP_BUFFER_DEDICATED,
            count: descs.len() as u32,
            buffers: descs.as_ptr(),
        };
        // SAFETY: a live producer and a ring of valid descriptors.
        assert_eq!(
            unsafe { lsp_offer_buffers(p, &offer) },
            LSP_OK,
            "{}",
            last_error()
        );
        assert_eq!(backend.imported.lock().unwrap().as_slice(), &[(1, 3)]);

        // A frame, asked for and shown, its meta handed through.
        shared.request_frame();
        let mut slot = LspSlot::default();
        // SAFETY: a live producer.
        assert_eq!(unsafe { lsp_next_slot(p, 5000, &mut slot) }, LSP_OK);
        let meta = b"camera";
        let frame = LspFrame {
            struct_size: std::mem::size_of::<LspFrame>() as u32,
            generation: slot.generation,
            slot: slot.slot,
            meta_len: meta.len() as u32,
            meta: meta.as_ptr(),
            t_sim: 0,
            t_submit: 0,
            t_gpu_done: 0,
            input_seq: 0,
        };
        let mut number = 0u64;
        // SAFETY: a slot this producer holds.
        assert_eq!(unsafe { lsp_frame_ready(p, &frame, &mut number) }, LSP_OK);
        wait_for("the C frame", || {
            shared.frame().is_some_and(|f| f.frame == number)
        });
        assert_eq!(shared.frame().unwrap().meta, meta);

        // A slot taken and handed back unused.
        shared.request_frame();
        // SAFETY: a live producer.
        assert_eq!(unsafe { lsp_next_slot(p, 5000, &mut slot) }, LSP_OK);
        // SAFETY: as above.
        assert_eq!(unsafe { lsp_abandon_slot(p, slot) }, LSP_OK);

        // Everything the consumer sends arrives as flattened events.
        shared.send_input(InputEvent::PointerMove(pointer(3.0)));
        let e = poll_for(p, LSP_EVENT_INPUT);
        assert_eq!(
            (e.input_kind, e.x, e.dx),
            (LSP_INPUT_POINTER_MOVE, 3.0, 1.0)
        );
        shared.send_input(InputEvent::PointerDown(pointer(4.0)));
        assert_eq!(
            poll_for(p, LSP_EVENT_INPUT).input_kind,
            LSP_INPUT_POINTER_DOWN
        );
        shared.send_input(InputEvent::PointerUp(pointer(4.0)));
        assert_eq!(
            poll_for(p, LSP_EVENT_INPUT).input_kind,
            LSP_INPUT_POINTER_UP
        );
        shared.send_input(InputEvent::Wheel {
            x: 1.0,
            y: 1.0,
            dx: 0.0,
            dy: 3.0,
            unit: 1,
        });
        let e = poll_for(p, LSP_EVENT_INPUT);
        assert_eq!(
            (e.input_kind, e.dy, e.wheel_unit),
            (LSP_INPUT_WHEEL, 3.0, 1)
        );
        shared.send_input(InputEvent::Key {
            code: 5,
            pressed: true,
            repeat: false,
            modifiers: 1,
            key: "w".into(),
        });
        let e = poll_for(p, LSP_EVENT_INPUT);
        assert_eq!(
            (e.input_kind, e.key_code, e.pressed, e.modifiers),
            (LSP_INPUT_KEY, 5, 1, 1)
        );
        // SAFETY: text stays valid until the next call on the producer.
        assert_eq!(unsafe { CStr::from_ptr(e.text) }.to_str(), Ok("w"));
        shared.send_input(InputEvent::Text("hi".into()));
        let e = poll_for(p, LSP_EVENT_INPUT);
        assert_eq!(e.input_kind, LSP_INPUT_TEXT);
        // SAFETY: as above.
        assert_eq!(unsafe { CStr::from_ptr(e.text) }.to_str(), Ok("hi"));
        for (input, kind) in [
            (InputEvent::Focus(true), LSP_INPUT_FOCUS),
            (InputEvent::Lock(true), LSP_INPUT_LOCK),
            (InputEvent::Leave, LSP_INPUT_LEAVE),
        ] {
            shared.send_input(input);
            assert_eq!(poll_for(p, LSP_EVENT_INPUT).input_kind, kind);
        }
        shared.set_visible(false);
        assert_eq!(poll_for(p, LSP_EVENT_VISIBILITY).flag, 0);
        shared.set_visible(true);
        assert!(shared.send_user(b"bytes"));
        let e = poll_for(p, LSP_EVENT_USER);
        // SAFETY: data_len bytes stay valid until the next call.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(e.data, e.data_len as usize) },
            b"bytes"
        );
        shared.set_wanted(Wanted {
            width: 32,
            height: 24,
            scale: 2.0,
        });
        let e = poll_for(p, LSP_EVENT_RESIZE);
        assert_eq!((e.width, e.height, e.scale), (32, 24, 2.0));

        // What the producer asks of the consumer's app.
        // SAFETY: a live producer.
        assert_eq!(unsafe { lsp_set_cursor(p, 2) }, LSP_OK);
        // SAFETY: as above.
        assert_eq!(unsafe { lsp_request_pointer_lock(p, 1) }, LSP_OK);
        let bytes = b"to the app";
        // SAFETY: a live producer and len readable bytes.
        assert_eq!(
            unsafe { lsp_send_user(p, bytes.as_ptr(), bytes.len() as u32) },
            LSP_OK
        );
        let mut seen = Vec::new();
        wait_happened(&shared, &mut seen, |seen| {
            seen.iter().any(|h| matches!(h, Happened::Cursor(_)))
                && seen
                    .iter()
                    .any(|h| matches!(h, Happened::PointerLock(true)))
                && seen
                    .iter()
                    .any(|h| matches!(h, Happened::User(b) if b == bytes))
        });

        // Arguments it refuses.
        // SAFETY: the null producer and out pointers are the cases under test.
        unsafe {
            assert_eq!(lsp_poll(std::ptr::null_mut(), &mut event()), LSP_ERR_ARG);
            assert_eq!(lsp_poll(p, std::ptr::null_mut()), LSP_ERR_ARG);
            assert_eq!(lsp_next_slot(p, 0, std::ptr::null_mut()), LSP_ERR_ARG);
            assert_eq!(lsp_offer_buffers(p, std::ptr::null()), LSP_ERR_ARG);
            assert_eq!(
                lsp_frame_ready(p, std::ptr::null(), &mut number),
                LSP_ERR_ARG
            );
            assert_eq!(lsp_send_user(p, std::ptr::null(), 4), LSP_ERR_ARG);
        }

        // The consumer ends the session; the producer hears it.
        shared.disconnect();
        let e = poll_for(p, LSP_EVENT_CLOSED);
        assert_ne!(e.reason, 0);
        // SAFETY: the producer is closed once and not used after.
        unsafe { lsp_close(p) };
        // SAFETY: closing nothing is allowed.
        unsafe { lsp_close(std::ptr::null_mut()) };
        drop(files);
        shared.close();
    }
}
