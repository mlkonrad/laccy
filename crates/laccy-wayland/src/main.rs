//! laccy-wayland does the GNOME Shell extension's work on other Wayland
//! compositors, like Sway and Hyprland: it sends each copy to laccyd, and
//! sets the clipboard and pastes when the popup asks. Start it with the
//! session, for example from the compositor's config.

mod wayland;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gio::glib;
use gio::prelude::*;
use laccy_core::config::{self, Config};
use laccy_core::{
    DAEMON_BUS_NAME, DAEMON_INTERFACE, DAEMON_OBJECT_PATH, FORMAT_MIME_TYPES, IMAGE_MIME_TYPES,
    PICKED_MIME_TYPE, WAYLAND_BUS_NAME, WAYLAND_INTERFACE, WAYLAND_OBJECT_PATH, files,
};
use wayland::Wayland;

const INTROSPECTION: &str = r#"
<node>
  <interface name="io.github.mlkonrad.Laccy.Clipboard1">
    <method name="SetClipboard">
      <arg name="text" type="s" direction="in"/>
      <arg name="paste" type="b" direction="in"/>
    </method>
    <method name="SetContent">
      <arg name="mime_type" type="s" direction="in"/>
      <arg name="data" type="ay" direction="in"/>
      <arg name="paste" type="b" direction="in"/>
    </method>
    <method name="Paste"/>
    <method name="ClearClipboard"/>
  </interface>
</node>
"#;

/// Set by password managers on copied secrets.
const SECRET_MIME_TYPE: &str = "x-kde-passwordManagerHint";
/// Text types, most preferred first. Picked text is offered as all of them.
const TEXT_MIME_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

/// Time for the compositor to give focus back to the previous window after
/// the popup closed. Ctrl+V sent sooner would go to the closing popup.
const PASTE_DELAY: Duration = Duration::from_millis(150);

type Shared = Rc<RefCell<Wayland>>;

fn main() -> glib::ExitCode {
    let wayland = match Wayland::connect() {
        Ok(wayland) => Rc::new(RefCell::new(wayland)),
        Err(err) => {
            eprintln!("laccy-wayland: {err}");
            return glib::ExitCode::FAILURE;
        }
    };
    let bus = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(bus) => bus,
        Err(err) => {
            eprintln!("laccy-wayland: cannot connect to the session bus: {err}");
            return glib::ExitCode::FAILURE;
        }
    };
    let settings = config::settings();
    if settings.is_none() {
        eprintln!("laccy-wayland: Laccy's settings schema was not found; using default settings");
    }

    let main_loop = glib::MainLoop::new(None, false);
    if let Err(err) = export(&bus, &wayland) {
        eprintln!("laccy-wayland: cannot export {WAYLAND_OBJECT_PATH}: {err}");
        return glib::ExitCode::FAILURE;
    }
    let owner_id = gio::bus_own_name_on_connection(
        &bus,
        WAYLAND_BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        |_, _| {},
        glib::clone!(
            #[strong]
            main_loop,
            move |_, name| {
                eprintln!(
                    "laccy-wayland: lost the D-Bus name {name} (another laccy-wayland is running?)"
                );
                main_loop.quit();
            }
        ),
    );

    let fd = wayland.borrow().fd();
    // Ends itself, with the main loop, when the compositor goes away.
    glib_unix::unix_fd_add_local(
        fd,
        glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
        glib::clone!(
            #[strong]
            main_loop,
            move |_, _| {
                let copies = match wayland.borrow_mut().dispatch() {
                    Ok(copies) => copies,
                    Err(err) => {
                        eprintln!("laccy-wayland: {err}");
                        main_loop.quit();
                        return glib::ControlFlow::Break;
                    }
                };
                for (offer, mime_types) in copies {
                    let (wayland, bus, settings) =
                        (Rc::clone(&wayland), bus.clone(), settings.clone());
                    glib::spawn_future_local(async move {
                        if let Err(err) =
                            record(&wayland, &bus, settings.as_ref(), &offer, &mime_types).await
                        {
                            eprintln!("laccy-wayland: cannot record a copy: {err}");
                        }
                        offer.destroy();
                        wayland.borrow().flush();
                    });
                }
                glib::ControlFlow::Continue
            }
        ),
    );
    main_loop.run();
    gio::bus_unown_name(owner_id);
    glib::ExitCode::FAILURE
}

/// Exports the methods the popup calls, the same as the extension's.
fn export(bus: &gio::DBusConnection, wayland: &Shared) -> Result<(), glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(INTROSPECTION).expect("introspection XML is valid");
    let interface = node
        .lookup_interface(WAYLAND_INTERFACE)
        .expect("interface is in the XML");
    let wayland = Rc::clone(wayland);
    bus.register_object(WAYLAND_OBJECT_PATH, &interface)
        .method_call(move |connection, sender, _, _, method, params, invocation| {
            // Otherwise any program on the session bus could replace the
            // clipboard or type into other apps.
            if !sender.is_some_and(|sender| is_laccy(&connection, sender)) {
                eprintln!("laccy-wayland: refused {method} from {}, which is not laccy", sender.unwrap_or("?"));
                invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "Only the Laccy popup may call this method");
                return;
            }
            let Some(paste) = handle(&wayland.borrow(), method, &params) else {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", method);
                return;
            };
            invocation.return_value(None);
            if paste {
                let wayland = Rc::clone(&wayland);
                glib::timeout_add_local_once(PASTE_DELAY, move || {
                    if !wayland.borrow().paste() {
                        eprintln!("laccy-wayland: not pasting, because the compositor has no virtual keyboards");
                    }
                });
            }
        })
        .build()
        .map(|_| ())
}

/// Runs a method call. Returns whether to paste afterwards, or `None` for bad arguments.
fn handle(wayland: &Wayland, method: &str, params: &glib::Variant) -> Option<bool> {
    match method {
        "SetClipboard" => {
            let (text, paste) = params.get::<(String, bool)>()?;
            wayland.set_clipboard(&TEXT_MIME_TYPES, text.into_bytes());
            Some(paste)
        }
        "SetContent" => {
            let mime_type = params.try_child_value(0)?.get::<String>()?;
            let data = params
                .try_child_value(1)?
                .fixed_array::<u8>()
                .ok()?
                .to_vec();
            let paste = params.try_child_value(2)?.get::<bool>()?;
            if mime_type.is_empty() {
                return None;
            }
            wayland.set_clipboard(&[&mime_type], data);
            Some(paste)
        }
        "Paste" => Some(true),
        "ClearClipboard" => {
            wayland.clear_clipboard();
            Some(false)
        }
        _ => None,
    }
}

/// Whether `sender` runs the `laccy` binary next to this one.
fn is_laccy(connection: &gio::DBusConnection, sender: &str) -> bool {
    std::env::current_exe()
        .map(|exe| laccy_core::caller_runs(connection, sender, &exe.with_file_name("laccy")))
        .unwrap_or(false)
}

/// Sends a new copy to laccyd, choosing what to read the way the extension does.
async fn record(
    wayland: &Shared,
    bus: &gio::DBusConnection,
    settings: Option<&gio::Settings>,
    offer: &wayland::Offer,
    mime_types: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let has = |mime_type: &str| mime_types.iter().any(|offered| offered == mime_type);
    if has(SECRET_MIME_TYPE) || has(PICKED_MIME_TYPE) {
        return Ok(());
    }
    if settings.is_some_and(|settings| settings.boolean(config::PAUSED_KEY)) {
        return Ok(());
    }
    let config = settings.map_or_else(Config::default, Config::from_settings);
    let read = |mime_type: &'static str, max_bytes: usize| {
        read_offer(wayland, offer, mime_type, max_bytes)
    };

    // File managers also offer paths as text, so check files first.
    if has(files::MIME_TYPE) {
        let uris: Vec<String> = read(files::MIME_TYPE, config.max_item_bytes)
            .await?
            .map(|data| {
                String::from_utf8_lossy(&data)
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        // Non-file URIs, like links, are recorded as text.
        if !uris.is_empty() && uris.iter().all(|uri| uri.starts_with("file://")) {
            if config.record_files {
                call_daemon(bus, "AddFiles", (uris, "").to_variant()).await?;
            }
            return Ok(());
        }
    }

    let image_type = IMAGE_MIME_TYPES
        .into_iter()
        .find(|mime_type| has(mime_type));
    if let Some(image_type) =
        image_type.filter(|_| !TEXT_MIME_TYPES.iter().any(|mime_type| has(mime_type)))
    {
        if config.record_images {
            if let Some(data) = read(image_type, config.max_image_bytes).await? {
                let params = glib::Variant::tuple_from_iter([
                    glib::Variant::array_from_fixed_array(&data),
                    image_type.to_variant(),
                    "".to_variant(),
                ]);
                call_daemon(bus, "AddImage", params).await?;
            }
        }
        return Ok(());
    }

    if !config.record_text {
        return Ok(());
    }
    let Some(text_type) = TEXT_MIME_TYPES.into_iter().find(|mime_type| has(mime_type)) else {
        return Ok(());
    };
    let Some(text) = read(text_type, config.max_item_bytes).await? else {
        return Ok(());
    };
    let text = String::from_utf8_lossy(&text).into_owned();
    if text.is_empty() {
        return Ok(());
    }
    let mut formats = Vec::new();
    for format_type in FORMAT_MIME_TYPES
        .into_iter()
        .filter(|mime_type| has(mime_type))
    {
        if let Some(data) = read(format_type, config.max_item_bytes)
            .await?
            .filter(|data| !data.is_empty())
        {
            formats.push(glib::Variant::from_dict_entry(
                &format_type.to_variant(),
                &glib::Variant::array_from_fixed_array(&data),
            ));
        }
    }
    if formats.is_empty() {
        return call_daemon(bus, "AddText", (text, "").to_variant()).await;
    }
    let params = glib::Variant::tuple_from_iter([
        text.to_variant(),
        glib::Variant::array_from_iter_with_type(
            glib::VariantTy::new("{say}").expect("valid type"),
            formats,
        ),
        "".to_variant(),
    ]);
    call_daemon(bus, "AddFormattedText", params).await
}

/// The copy in `offer` as `mime_type`, or `None` if it's larger than
/// `max_bytes` or the app didn't hand it over in time.
async fn read_offer(
    wayland: &Shared,
    offer: &wayland::Offer,
    mime_type: &str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    let fd = wayland.borrow().receive(offer, mime_type)?;
    gio::spawn_blocking(move || wayland::read_pipe(&fd, max_bytes))
        .await
        .map_err(|_| "the clipboard reader panicked".into())
}

async fn call_daemon(
    bus: &gio::DBusConnection,
    method: &str,
    params: glib::Variant,
) -> Result<(), Box<dyn std::error::Error>> {
    bus.call_future(
        Some(DAEMON_BUS_NAME),
        DAEMON_OBJECT_PATH,
        DAEMON_INTERFACE,
        method,
        Some(&params),
        None,
        gio::DBusCallFlags::NONE,
        10_000,
    )
    .await?;
    Ok(())
}
