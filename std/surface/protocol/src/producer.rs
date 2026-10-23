//! The producer's side of a session: connect, hand over a ring of buffers,
//! render into the slots the consumer frees, and announce finished frames.
//!
//! [`Producer`] owns the bookkeeping the protocol requires of a producer:
//! it never hands out a slot the consumer still reads, it paces frames to
//! the consumer's requests in paced mode, it answers pings, and it queues
//! everything else the consumer sends (input, resizes, visibility, user
//! messages) as [`ProducerEvent`]s for the engine to take with
//! [`Producer::poll_event`].
//!
//! The engine does the GPU work: it allocates and exports images, renders,
//! waits for its own GPU, then calls [`Producer::frame_ready`]. Nothing here
//! touches a graphics API.

use crate::endpoint::{Endpoint, Transport};
use crate::unix::{Connection, Received, Receiver, Sender, TransportError};
use crate::wire::{
    AdapterId, Api, Buffers, ByeReason, FrameReady, InputEvent, MAJOR, MINOR, Message, Pacing,
    Reject, Welcome,
};
use std::collections::{HashMap, VecDeque};
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// How long [`Producer::connect`] and [`Producer::offer_buffers`] wait for
/// the consumer's answer while the consumer says nothing at all. A consumer
/// still preparing a ring keeps pinging a quiet producer, and every message
/// it sends restarts the wait, so a slow GPU delays the answer without
/// failing it.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

/// What a producer says about itself in its `Hello`.
#[derive(Clone, Debug)]
pub struct ProducerHello {
    /// The API it renders with.
    pub api: Api,
    /// The GPU it renders on.
    pub adapter: AdapterId,
    /// Engine name and version.
    pub engine: String,
    /// Capability bits; none are defined in version 1.
    pub caps: u32,
}

/// Something the consumer sent that the engine acts on.
#[derive(Clone, Debug, PartialEq)]
pub enum ProducerEvent {
    /// Make frames at this size from now on: offer a new ring.
    Resize {
        /// Width in texture pixels.
        width: u32,
        /// Height in texture pixels.
        height: u32,
        /// Device pixels per logical pixel.
        scale: f32,
    },
    /// Input, in the order it happened.
    Input {
        /// Its sequence number; echo the newest applied in
        /// [`FrameReady::input_seq`].
        seq: u64,
        /// When it happened, monotonic nanoseconds.
        time: u64,
        /// What happened.
        event: InputEvent,
    },
    /// Whether anyone can see the frames.
    Visibility(bool),
    /// Bytes from the application.
    User(Vec<u8>),
    /// The consumer's device changed: every buffer is gone; offer a new ring.
    Reset,
    /// Every slot of this generation is back; free its buffers.
    Retired(u32),
    /// The consumer refused a ring.
    BuffersRejected {
        /// The ring's generation.
        generation: u32,
        /// Words for a person.
        detail: String,
    },
    /// The session ended.
    Closed {
        /// Why, when the consumer said.
        reason: Option<ByeReason>,
        /// Words for a person.
        detail: String,
    },
}

/// What can go wrong for a producer.
#[derive(Debug)]
pub enum ProducerError {
    /// The endpoint string does not parse, or names a transport this build
    /// lacks.
    Endpoint(String),
    /// The socket failed.
    Transport(TransportError),
    /// The consumer refused this producer.
    Rejected(Reject),
    /// The consumer did not answer in time.
    Timeout,
    /// The session is over.
    Closed,
    /// A call broke the protocol's rules (a slot that is not this
    /// producer's to fill, a ring of no buffers).
    Misuse(&'static str),
}

impl std::fmt::Display for ProducerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProducerError::Endpoint(e) => write!(f, "{e}"),
            ProducerError::Transport(e) => write!(f, "{e}"),
            ProducerError::Rejected(r) => {
                write!(
                    f,
                    "the consumer refused: {} ({})",
                    r.reason.as_str(),
                    r.detail
                )
            }
            ProducerError::Timeout => f.write_str("the consumer did not answer in time"),
            ProducerError::Closed => f.write_str("the session is over"),
            ProducerError::Misuse(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for ProducerError {}

impl From<TransportError> for ProducerError {
    fn from(e: TransportError) -> Self {
        ProducerError::Transport(e)
    }
}

/// Where one slot of a ring is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    /// The producer may render into it.
    Free,
    /// Handed out by [`Producer::next_slot`], not announced yet.
    Rendering,
    /// Announced; the consumer may be reading it.
    WithConsumer,
}

/// A slot the engine renders the next frame into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The ring generation.
    pub generation: u32,
    /// The slot index in that ring.
    pub slot: u32,
}

/// The producer's side of one session.
pub struct Producer {
    sender: Sender,
    receiver: Receiver,
    welcome: Welcome,
    /// Rings by generation; the newest is the one frames go to.
    rings: HashMap<u32, Vec<SlotState>>,
    current: Option<u32>,
    /// Frame requests not answered yet (paced mode).
    requests: u64,
    events: VecDeque<ProducerEvent>,
    closed: bool,
    next_frame: u64,
    last_input: u64,
}

fn now_ns() -> u64 {
    crate::monotonic_ns()
}

impl Producer {
    /// Connect to the consumer the endpoint names and introduce this
    /// producer. Returns once the consumer welcomed it.
    ///
    /// # Errors
    ///
    /// The endpoint is unusable, the socket failed, the consumer refused,
    /// or it did not answer within [`ANSWER_TIMEOUT`].
    pub fn connect(endpoint: &str, hello: &ProducerHello) -> Result<Producer, ProducerError> {
        let endpoint: Endpoint = endpoint
            .parse()
            .map_err(|e: crate::endpoint::EndpointError| ProducerError::Endpoint(e.to_string()))?;
        let Transport::Unix(path) = &endpoint.transport;
        let connection =
            Connection::connect(path).map_err(|e| ProducerError::Transport(e.into()))?;
        let (sender, mut receiver) = connection.split();
        sender.send(
            &Message::Hello(crate::wire::Hello {
                major: MAJOR,
                minor: MINOR,
                api: hello.api,
                pid: std::process::id(),
                caps: hello.caps,
                token: endpoint.token,
                adapter: hello.adapter,
                channel: endpoint.channel.clone(),
                engine: hello.engine.clone(),
            }),
            &[],
        )?;
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match receiver.recv(Some(left))? {
                Received::Message(Message::Welcome(welcome), _) => {
                    return Ok(Producer {
                        sender,
                        receiver,
                        welcome,
                        rings: HashMap::new(),
                        current: None,
                        requests: 0,
                        events: VecDeque::new(),
                        closed: false,
                        next_frame: 1,
                        last_input: 0,
                    });
                }
                Received::Message(Message::Reject(reject), _) => {
                    return Err(ProducerError::Rejected(reject));
                }
                Received::Message(Message::Ping(t), _) => {
                    sender.send(&Message::Pong(t), &[])?;
                }
                Received::Message(..) => {}
                Received::Timeout => return Err(ProducerError::Timeout),
                Received::Closed => return Err(ProducerError::Closed),
            }
        }
    }

    /// What the consumer said when it welcomed this producer: the size it
    /// wants, the pacing, the ring sizes and formats it takes.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    /// Hand the consumer a new ring, the buffers' OS handles in `fds` (one
    /// per buffer, in order). Waits for the consumer to import it, for as
    /// long as the consumer keeps talking (see [`ANSWER_TIMEOUT`]). The ring
    /// replaces the previous one for new frames; slots of older rings come
    /// back as the consumer stops showing them.
    ///
    /// # Errors
    ///
    /// The ring is empty or larger than the consumer takes, the consumer
    /// refused it (also queued as [`ProducerEvent::BuffersRejected`]), or
    /// the session ended.
    pub fn offer_buffers(&mut self, buffers: Buffers, fds: &[RawFd]) -> Result<(), ProducerError> {
        let count = buffers.buffers.len();
        if count == 0 || count != fds.len() {
            return Err(ProducerError::Misuse(
                "a ring needs one descriptor for each of at least one buffer",
            ));
        }
        let generation = buffers.generation;
        if self.rings.contains_key(&generation) {
            return Err(ProducerError::Misuse(
                "that ring generation was offered already",
            ));
        }
        self.sender.send(&Message::Buffers(buffers), fds)?;
        loop {
            match self.receiver.recv(Some(ANSWER_TIMEOUT))? {
                Received::Message(Message::BuffersAccepted { generation: g }, _)
                    if g == generation =>
                {
                    self.rings.insert(generation, vec![SlotState::Free; count]);
                    self.current = Some(generation);
                    return Ok(());
                }
                Received::Message(
                    Message::BuffersRejected {
                        generation: g,
                        detail,
                        ..
                    },
                    _,
                ) if g == generation => {
                    self.events.push_back(ProducerEvent::BuffersRejected {
                        generation,
                        detail: detail.clone(),
                    });
                    return Err(ProducerError::Misuse("the consumer refused the ring"));
                }
                Received::Message(message, _) => self.handle(message)?,
                Received::Timeout => return Err(ProducerError::Timeout),
                Received::Closed => {
                    self.close_quietly(None, String::new());
                    return Err(ProducerError::Closed);
                }
            }
        }
    }

    /// The slot to render the next frame into, once one is free and (paced)
    /// the consumer asked for a frame. Waits at most `timeout`; `Ok(None)` when
    /// none came. Messages that arrive meanwhile are queued as events.
    ///
    /// # Errors
    ///
    /// No ring was offered yet, or the session ended.
    pub fn next_slot(&mut self, timeout: Duration) -> Result<Option<Slot>, ProducerError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain(Some(Duration::ZERO))?;
            if self.closed {
                return Err(ProducerError::Closed);
            }
            let Some(generation) = self.current else {
                return Err(ProducerError::Misuse("no ring was offered"));
            };
            let wanted = self.welcome.pacing == Pacing::Free || self.requests > 0;
            if wanted {
                let ring = self.rings.get_mut(&generation).expect("the current ring");
                if let Some(slot) = ring.iter().position(|s| *s == SlotState::Free) {
                    ring[slot] = SlotState::Rendering;
                    return Ok(Some(Slot {
                        generation,
                        slot: slot as u32,
                    }));
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.drain(Some(left.min(Duration::from_millis(50))))?;
        }
    }

    /// Announce that the frame in `slot` is finished: the engine's GPU work
    /// for it has completed. `meta` is up to 4 KiB of bytes the consumer
    /// hands its app with the frame. Returns the frame number.
    ///
    /// # Errors
    ///
    /// The slot was not handed out by [`Self::next_slot`], or the session
    /// ended.
    pub fn frame_ready(
        &mut self,
        slot: Slot,
        meta: &[u8],
        times: FrameTimes,
    ) -> Result<u64, ProducerError> {
        let state = self
            .rings
            .get_mut(&slot.generation)
            .and_then(|r| r.get_mut(slot.slot as usize))
            .ok_or(ProducerError::Misuse("no such slot"))?;
        if *state != SlotState::Rendering {
            return Err(ProducerError::Misuse("the slot was not handed out"));
        }
        *state = SlotState::WithConsumer;
        self.requests = self.requests.saturating_sub(1);
        let frame = self.next_frame;
        self.next_frame += 1;
        let gpu_done = if times.gpu_done == 0 {
            now_ns()
        } else {
            times.gpu_done
        };
        self.sender.send(
            &Message::FrameReady(FrameReady {
                frame,
                input_seq: times.input_seq.unwrap_or(self.last_input),
                t_sim: times.sim,
                t_submit: times.submit,
                t_gpu_done: gpu_done,
                generation: slot.generation,
                slot: slot.slot,
                meta: meta.to_vec(),
            }),
            &[],
        )?;
        Ok(frame)
    }

    /// Give back a slot [`Self::next_slot`] handed out without announcing a
    /// frame in it (the engine skipped the frame).
    pub fn abandon(&mut self, slot: Slot) {
        if let Some(state) = self
            .rings
            .get_mut(&slot.generation)
            .and_then(|r| r.get_mut(slot.slot as usize))
            && *state == SlotState::Rendering
        {
            *state = SlotState::Free;
        }
    }

    /// The next thing the consumer sent, without waiting.
    ///
    /// # Errors
    ///
    /// The socket failed.
    pub fn poll_event(&mut self) -> Result<Option<ProducerEvent>, ProducerError> {
        if self.events.is_empty() && !self.closed {
            self.drain(Some(Duration::ZERO))?;
        }
        Ok(self.events.pop_front())
    }

    /// Ask for the cursor over the surface: a [`crate::wire::cursor`] number.
    ///
    /// # Errors
    ///
    /// The session ended.
    pub fn set_cursor(&mut self, keyword: u16) -> Result<(), ProducerError> {
        self.send(&Message::Cursor { keyword })
    }

    /// Ask for the pointer to be locked to the surface, or let go. The
    /// consumer grants a lock only to a focused surface just after a press;
    /// an [`InputEvent::Lock`] says when it took effect.
    ///
    /// # Errors
    ///
    /// The session ended.
    pub fn request_pointer_lock(&mut self, wanted: bool) -> Result<(), ProducerError> {
        self.send(&Message::PointerLock { wanted })
    }

    /// Send bytes to the consumer's application, at most 60 KiB.
    ///
    /// # Errors
    ///
    /// Too many bytes, or the session ended.
    pub fn send_user(&mut self, bytes: &[u8]) -> Result<(), ProducerError> {
        self.send(&Message::User(bytes.to_vec()))
    }

    /// End the session.
    pub fn close(mut self, reason: ByeReason, detail: &str) {
        let _ = self.sender.send(
            &Message::Bye {
                reason,
                detail: detail.to_string(),
            },
            &[],
        );
        self.closed = true;
        self.receiver.shutdown();
    }

    fn send(&mut self, message: &Message) -> Result<(), ProducerError> {
        if self.closed {
            return Err(ProducerError::Closed);
        }
        self.sender.send(message, &[])?;
        Ok(())
    }

    fn close_quietly(&mut self, reason: Option<ByeReason>, detail: String) {
        if !self.closed {
            self.closed = true;
            self.events
                .push_back(ProducerEvent::Closed { reason, detail });
        }
    }

    /// Read what has arrived, waiting at most `timeout` for the first byte.
    fn drain(&mut self, timeout: Option<Duration>) -> Result<(), ProducerError> {
        let mut wait = timeout;
        while !self.closed {
            match self.receiver.recv(wait)? {
                Received::Message(message, _) => self.handle(message)?,
                Received::Timeout => break,
                Received::Closed => self.close_quietly(None, "the consumer went away".into()),
            }
            wait = Some(Duration::ZERO);
        }
        Ok(())
    }

    fn handle(&mut self, message: Message) -> Result<(), ProducerError> {
        match message {
            Message::FrameRequest { .. } => self.requests += 1,
            Message::Release {
                generation, slot, ..
            } => {
                if let Some(state) = self
                    .rings
                    .get_mut(&generation)
                    .and_then(|r| r.get_mut(slot as usize))
                    && *state == SlotState::WithConsumer
                {
                    *state = SlotState::Free;
                }
            }
            Message::RetireGeneration { generation } => {
                if Some(generation) != self.current {
                    self.rings.remove(&generation);
                }
                self.events.push_back(ProducerEvent::Retired(generation));
            }
            Message::Resize {
                width,
                height,
                scale,
            } => {
                self.welcome.width = width;
                self.welcome.height = height;
                self.welcome.scale = scale;
                self.events.push_back(ProducerEvent::Resize {
                    width,
                    height,
                    scale,
                });
            }
            Message::Input { seq, time, event } => {
                self.last_input = self.last_input.max(seq);
                self.events
                    .push_back(ProducerEvent::Input { seq, time, event });
            }
            Message::Visibility { visible } => {
                self.events.push_back(ProducerEvent::Visibility(visible));
            }
            Message::User(bytes) => self.events.push_back(ProducerEvent::User(bytes)),
            Message::Ping(t) => self.sender.send(&Message::Pong(t), &[])?,
            Message::Reset { .. } => {
                self.rings.clear();
                self.current = None;
                self.requests = 0;
                self.events.push_back(ProducerEvent::Reset);
            }
            Message::Bye { reason, detail } => {
                self.close_quietly(Some(reason), detail);
            }
            // Answers to calls in flight, and messages a consumer does not
            // send, are ignored here.
            _ => {}
        }
        Ok(())
    }
}

/// When a frame's work happened, monotonic nanoseconds; 0 for unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameTimes {
    /// The frame's simulation.
    pub sim: u64,
    /// Its GPU submit.
    pub submit: u64,
    /// Its GPU completion; 0 takes the moment of the call.
    pub gpu_done: u64,
    /// The newest input it reflects; `None` takes the newest input the
    /// producer has read.
    pub input_seq: Option<u64>,
}
