//! The Wayland side of laccy-wayland: watching the clipboard through
//! ext-data-control-v1, owning it with our own data sources, and typing
//! Ctrl+V with a virtual keyboard.

use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use laccy_core::PICKED_MIME_TYPE;
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop, event_created_child,
};
pub use wayland_protocols::ext::data_control::v1::client::ext_data_control_offer_v1::ExtDataControlOfferV1 as Offer;
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1 as Device},
    ext_data_control_manager_v1::ExtDataControlManagerV1 as Manager,
    ext_data_control_offer_v1,
    ext_data_control_source_v1::{self, ExtDataControlSourceV1 as Source},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1 as KeyboardManager,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1 as Keyboard,
};

/// How long an app may take to hand over a copy, or to take one of ours.
/// An app that never closes its end of the pipe is given up on.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(10);

/// A standard US keymap, which the compositor expands from its own keyboard
/// data. Compositors switch to a virtual keyboard's keymap while it types,
/// so the user's layout doesn't matter. A keymap holding only Ctrl and V
/// isn't enough: GTK 3 apps like Mousepad ignored its Ctrl+V.
const KEYMAP: &str = r#"xkb_keymap {
    xkb_keycodes { include "evdev+aliases(qwerty)" };
    xkb_types { include "complete" };
    xkb_compatibility { include "complete" };
    xkb_symbols { include "pc+us+inet(evdev)" };
};
"#;
/// Evdev codes of Left Ctrl and V: their XKB keycodes (37, 55) minus 8.
const KEY_LEFTCTRL: u32 = 29;
const KEY_V: u32 = 47;
/// The Control bit in the keymap's modifier mask.
const CONTROL_MASK: u32 = 4;

/// Mime types offered by one clipboard offer, as the compositor lists them.
type OfferMimeTypes = Mutex<Vec<String>>;

/// What the event handlers collect between dispatches.
#[derive(Default)]
pub struct State {
    /// New clipboard contents, in the order they were copied.
    copies: Vec<(Offer, Vec<String>)>,
}

pub struct Wayland {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
    handle: QueueHandle<State>,
    manager: Manager,
    device: Device,
    keyboard: Option<Keyboard>,
}

impl Wayland {
    pub fn connect() -> Result<Self, String> {
        let connection = Connection::connect_to_env()
            .map_err(|err| format!("cannot connect to Wayland: {err}"))?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .map_err(|err| format!("cannot list Wayland globals: {err}"))?;
        let handle = queue.handle();
        let seat: WlSeat = globals
            .bind(&handle, 1..=1, ())
            .map_err(|err| format!("no Wayland seat: {err}"))?;
        let manager: Manager = globals.bind(&handle, 1..=1, ()).map_err(|_| {
            "the compositor doesn't offer ext-data-control-v1, which laccy-wayland needs to watch the clipboard"
                .to_owned()
        })?;
        let keyboard = match globals.bind::<KeyboardManager, _, _>(&handle, 1..=1, ()) {
            Ok(keyboards) => Some(new_keyboard(&keyboards, &seat, &handle)?),
            Err(_) => {
                eprintln!(
                    "laccy-wayland: the compositor doesn't offer zwp_virtual_keyboard_v1, so it can't paste"
                );
                None
            }
        };
        let device = manager.get_data_device(&seat, &handle, ());

        // The clipboard as it was at startup is announced first; it isn't a new copy.
        let mut state = State::default();
        queue
            .roundtrip(&mut state)
            .map_err(|err| format!("Wayland roundtrip failed: {err}"))?;
        for (offer, _) in state.copies.drain(..) {
            offer.destroy();
        }
        let wayland = Self {
            connection,
            queue,
            state,
            handle,
            manager,
            device,
            keyboard,
        };
        wayland.flush();
        Ok(wayland)
    }

    /// The file descriptor to watch for events.
    pub fn fd(&self) -> RawFd {
        self.connection.backend().poll_fd().as_raw_fd()
    }

    /// Reads and handles the events waiting on [`Self::fd`]. Returns the new
    /// copies; each offer must be destroyed once read.
    pub fn dispatch(&mut self) -> Result<Vec<(Offer, Vec<String>)>, String> {
        if let Some(guard) = self.queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(err))
                    if err.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(err) => return Err(format!("lost the Wayland connection: {err}")),
            }
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|err| format!("Wayland error: {err}"))?;
        self.flush();
        Ok(std::mem::take(&mut self.state.copies))
    }

    pub fn flush(&self) {
        if let Err(err) = self.connection.flush() {
            eprintln!("laccy-wayland: cannot send to the compositor: {err}");
        }
    }

    /// Asks the app that copied `offer` to write it as `mime_type`. The
    /// returned pipe end gives the data; read it with [`read_pipe`].
    pub fn receive(&self, offer: &Offer, mime_type: &str) -> std::io::Result<OwnedFd> {
        let (read, write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        offer.receive(mime_type.to_owned(), write.as_fd());
        drop(write);
        self.flush();
        Ok(read)
    }

    /// Puts `data` on the clipboard, offered as each of `mime_types`. It also
    /// offers [`PICKED_MIME_TYPE`], so this copy isn't recorded again.
    pub fn set_clipboard(&self, mime_types: &[&str], data: Vec<u8>) {
        let source = self
            .manager
            .create_data_source(&self.handle, Arc::new(data));
        for mime_type in mime_types.iter().chain([&PICKED_MIME_TYPE]) {
            source.offer((*mime_type).to_owned());
        }
        self.device.set_selection(Some(&source));
        self.flush();
    }

    pub fn clear_clipboard(&self) {
        self.device.set_selection(None);
        self.flush();
    }

    /// Types Ctrl+V into the focused window. False without a virtual keyboard.
    pub fn paste(&self) -> bool {
        let Some(keyboard) = &self.keyboard else {
            return false;
        };
        // Wrapping is fine: only the order of these times matters.
        let time = millis();
        keyboard.key(time, KEY_LEFTCTRL, 1);
        keyboard.modifiers(CONTROL_MASK, 0, 0, 0);
        keyboard.key(time, KEY_V, 1);
        keyboard.key(time, KEY_V, 0);
        keyboard.key(time, KEY_LEFTCTRL, 0);
        keyboard.modifiers(0, 0, 0, 0);
        self.flush();
        true
    }
}

fn millis() -> u32 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u32
}

fn new_keyboard(
    keyboards: &KeyboardManager,
    seat: &WlSeat,
    handle: &QueueHandle<State>,
) -> Result<Keyboard, String> {
    let keyboard = keyboards.create_virtual_keyboard(seat, handle, ());
    let keymap = rustix::fs::memfd_create("laccy-keymap", rustix::fs::MemfdFlags::CLOEXEC)
        .map_err(|err| format!("cannot create the keymap: {err}"))?;
    let mut file = std::fs::File::from(keymap);
    // The size includes the terminating NUL that the protocol expects.
    file.write_all(KEYMAP.as_bytes())
        .and_then(|()| file.write_all(b"\0"))
        .map_err(|err| format!("cannot write the keymap: {err}"))?;
    const XKB_V1: u32 = 1;
    keyboard.keymap(XKB_V1, file.as_fd(), KEYMAP.len() as u32 + 1);
    Ok(keyboard)
}

/// Reads all of `fd` in a blocking way, or `None` if it holds more than
/// `max_bytes`, fails, or doesn't finish within [`TRANSFER_TIMEOUT`].
pub fn read_pipe(fd: &OwnedFd, max_bytes: usize) -> Option<Vec<u8>> {
    let deadline = Instant::now() + TRANSFER_TIMEOUT;
    let mut data = Vec::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if !wait_for(fd, PollFlags::IN, deadline) {
            return None;
        }
        match rustix::io::read(fd, &mut buffer) {
            Ok(0) => return Some(data),
            Ok(read) if data.len() + read > max_bytes => return None,
            Ok(read) => data.extend_from_slice(&buffer[..read]),
            Err(Errno::INTR | Errno::AGAIN) => {}
            Err(_) => return None,
        }
    }
}

/// Writes all of `data` to `fd`, giving up after [`TRANSFER_TIMEOUT`].
fn write_pipe(fd: &OwnedFd, data: &[u8]) {
    let deadline = Instant::now() + TRANSFER_TIMEOUT;
    let mut written = 0;
    while written < data.len() {
        if !wait_for(fd, PollFlags::OUT, deadline) {
            return;
        }
        match rustix::io::write(fd, &data[written..]) {
            Ok(count) => written += count,
            Err(Errno::INTR | Errno::AGAIN) => {}
            // The app stopped reading, often on purpose.
            Err(_) => return,
        }
    }
}

/// Waits until `fd` is ready for `flags`; false once `deadline` passed.
fn wait_for(fd: &OwnedFd, flags: PollFlags, deadline: Instant) -> bool {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let timeout = Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let mut fds = [PollFd::new(fd, flags)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(Errno::INTR) => {}
            // Ready, or hung up: the read or write that follows tells which.
            Ok(_) => return true,
            Err(_) => return false,
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: Manager);
delegate_noop!(State: KeyboardManager);
delegate_noop!(State: Keyboard);

impl Dispatch<Device, ()> for State {
    fn event(
        state: &mut Self,
        _: &Device,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::Selection { id: Some(offer) } => {
                let mime_types = offer
                    .data::<OfferMimeTypes>()
                    .map(|types| types.lock().unwrap().clone())
                    .unwrap_or_default();
                state.copies.push((offer, mime_types));
            }
            // Only the clipboard is recorded, not text that was just selected.
            ext_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => {
                offer.destroy()
            }
            ext_data_control_device_v1::Event::Finished => {
                eprintln!("laccy-wayland: the compositor stopped sharing the clipboard");
                std::process::exit(1);
            }
            _ => {}
        }
    }

    event_created_child!(State, Device, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (Offer, OfferMimeTypes::default()),
    ]);
}

impl Dispatch<Offer, OfferMimeTypes> for State {
    fn event(
        _: &mut Self,
        _: &Offer,
        event: ext_data_control_offer_v1::Event,
        mime_types: &OfferMimeTypes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            mime_types.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<Source, Arc<Vec<u8>>> for State {
    fn event(
        _: &mut Self,
        source: &Source,
        event: ext_data_control_source_v1::Event,
        data: &Arc<Vec<u8>>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The marker type carries nothing; dropping `fd` closes it.
            ext_data_control_source_v1::Event::Send { mime_type, fd }
                if mime_type != PICKED_MIME_TYPE =>
            {
                let data = Arc::clone(data);
                std::thread::spawn(move || write_pipe(&fd, &data));
            }
            // Something else was copied since.
            ext_data_control_source_v1::Event::Cancelled => source.destroy(),
            _ => {}
        }
    }
}
