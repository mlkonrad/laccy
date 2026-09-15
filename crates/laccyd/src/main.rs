//! laccyd records clipboard changes reported by the GNOME Shell extension.
//! D-Bus activation starts it on the first copy. It has no UI, so it stays small.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::rc::Rc;

use gio::glib;
use laccy_core::config::{self, Config};
use laccy_core::history::{Content, Format, History};
use laccy_core::{DAEMON_BUS_NAME, DAEMON_INTERFACE, DAEMON_OBJECT_PATH, HISTORY_CHANGED_SIGNAL};

const INTROSPECTION: &str = r#"
<node>
  <interface name="io.github.mlkonrad.Laccy.Daemon1">
    <method name="AddText">
      <arg name="text" type="s" direction="in"/>
      <arg name="source_app" type="s" direction="in"/>
    </method>
    <method name="AddFormattedText">
      <arg name="text" type="s" direction="in"/>
      <arg name="formats" type="a{say}" direction="in"/>
      <arg name="source_app" type="s" direction="in"/>
    </method>
    <method name="AddFiles">
      <arg name="uris" type="as" direction="in"/>
      <arg name="source_app" type="s" direction="in"/>
    </method>
    <method name="AddImage">
      <arg name="data" type="ay" direction="in"/>
      <arg name="mime_type" type="s" direction="in"/>
      <arg name="source_app" type="s" direction="in"/>
    </method>
    <method name="Clear">
      <arg name="include_pinned" type="b" direction="in"/>
    </method>
    <signal name="HistoryChanged"/>
  </interface>
</node>
"#;

fn main() -> glib::ExitCode {
    let history = match History::open_default() {
        Ok(history) => Rc::new(RefCell::new(history)),
        Err(err) => {
            eprintln!("laccyd: cannot open history: {err}");
            return glib::ExitCode::FAILURE;
        }
    };

    // Changes made in the preferences window reach this object through the main loop.
    let settings = config::settings();
    if settings.is_none() {
        eprintln!("laccyd: the extension's settings schema was not found; using default settings");
    }

    let main_loop = glib::MainLoop::new(None, false);
    let owner_id = gio::bus_own_name(
        gio::BusType::Session,
        DAEMON_BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        move |connection, _| register(&connection, Rc::clone(&history), settings.clone()),
        |_, _| {},
        glib::clone!(
            #[strong]
            main_loop,
            move |_, name| {
                eprintln!("laccyd: lost the D-Bus name {name} (another laccyd is running, or the session ended)");
                main_loop.quit();
            }
        ),
    );
    main_loop.run();
    gio::bus_unown_name(owner_id);
    glib::ExitCode::FAILURE
}

fn register(connection: &gio::DBusConnection, history: Rc<RefCell<History>>, settings: Option<gio::Settings>) {
    let node = gio::DBusNodeInfo::for_xml(INTROSPECTION).expect("introspection XML is valid");
    let interface = node.lookup_interface(DAEMON_INTERFACE).expect("interface is in the XML");

    let registration = connection
        .register_object(DAEMON_OBJECT_PATH, &interface)
        .method_call(move |connection, _, _, _, method, params, invocation| {
            // GDBus has already checked the method and its argument types against the XML.
            if method == "Clear" {
                let include_pinned = params.get::<(bool,)>().is_some_and(|(include_pinned,)| include_pinned);
                match history.borrow().clear(include_pinned) {
                    Ok(_) => {
                        invocation.return_value(None);
                        announce_change(&connection);
                    }
                    Err(err) => {
                        eprintln!("laccyd: cannot clear history: {err}");
                        invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &err.to_string());
                    }
                }
                return;
            }
            let Some((content, formats, source_app)) = parse_call(method, &params) else {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", method);
                return;
            };
            let config = settings.as_ref().map_or_else(Config::default, Config::from_settings);
            if !config.should_record(&content, &source_app) {
                invocation.return_value(None);
                return;
            }
            let formats = config.formats_to_record(formats);
            // Making an image thumbnail takes a moment; keep answering other calls meanwhile.
            let history = Rc::clone(&history);
            glib::spawn_future_local(async move {
                let is_image = matches!(content, Content::Image { .. });
                match record(&history, &config, content, &formats, &source_app).await {
                    Ok(()) => {
                        invocation.return_value(None);
                        announce_change(&connection);
                    }
                    Err(err) => {
                        eprintln!("laccyd: cannot store clipboard item: {err}");
                        invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &err.to_string());
                    }
                }
                if is_image {
                    // GDBus frees the call's copy of the image in its own thread after replying.
                    glib::timeout_add_seconds_local_once(1, move || release_memory(&history.borrow()));
                }
            });
        })
        .build();

    if let Err(err) = registration {
        eprintln!("laccyd: cannot export {DAEMON_OBJECT_PATH}: {err}");
        std::process::exit(1);
    }
}

/// Tells open popups (and the extension) that the history changed.
fn announce_change(connection: &gio::DBusConnection) {
    if let Err(err) = connection.emit_signal(None, DAEMON_OBJECT_PATH, DAEMON_INTERFACE, HISTORY_CHANGED_SIGNAL, None) {
        eprintln!("laccyd: cannot announce the history change: {err}");
    }
}

/// The copied content, its formatted versions and the source app of a D-Bus call.
fn parse_call(method: &str, params: &glib::Variant) -> Option<(Content, Vec<Format>, String)> {
    match method {
        "AddText" => params
            .get::<(String, String)>()
            .map(|(text, source_app)| (Content::Text(text), Vec::new(), source_app)),
        "AddFormattedText" => {
            let text = params.try_child_value(0)?.get::<String>()?;
            let entries = params.try_child_value(1)?;
            let formats = (0..entries.n_children())
                .map(|index| {
                    let entry = entries.try_child_value(index)?;
                    let mime_type = entry.try_child_value(0)?.get::<String>()?;
                    let data = entry.try_child_value(1)?.fixed_array::<u8>().ok()?.to_vec();
                    Some(Format { mime_type, data })
                })
                .collect::<Option<Vec<_>>>()?;
            let source_app = params.try_child_value(2)?.get::<String>()?;
            Some((Content::Text(text), formats, source_app))
        }
        "AddFiles" => params
            .get::<(Vec<String>, String)>()
            .map(|(uris, source_app)| (Content::Files(uris), Vec::new(), source_app)),
        "AddImage" => {
            // Not `get`, which would convert the image one byte variant at a time.
            let data = params.try_child_value(0)?.fixed_array::<u8>().ok()?.to_vec();
            let mime_type = params.try_child_value(1)?.get::<String>()?;
            let source_app = params.try_child_value(2)?.get::<String>()?;
            Some((Content::Image { mime_type, data }, Vec::new(), source_app))
        }
        _ => None,
    }
}

/// Stores a copy that [`Config::should_record`] accepted, with the formats
/// [`Config::formats_to_record`] kept.
async fn record(
    history: &RefCell<History>,
    config: &Config,
    content: Content,
    formats: &[Format],
    source_app: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Made before storing, so images the popup couldn't show are never recorded.
    let thumbnail = match &content {
        Content::Image { data, .. } => Some(thumbnail(data).await?),
        _ => None,
    };
    let source_app = (!source_app.is_empty()).then_some(source_app);
    let mut history = history.borrow_mut();
    let id = history.add(&content, source_app, config.history_size)?;
    if let Some(png) = thumbnail {
        history.set_thumbnail(id, &png)?;
    }
    // A copy of the same text without formatting keeps the formats it had.
    if !formats.is_empty() {
        history.set_formats(id, formats)?;
    }
    Ok(())
}

/// A PNG thumbnail of the image in `data`, made by `laccy --thumbnail`. The
/// image libraries keep memory after decoding, so they never load in this
/// long-running process.
async fn thumbnail(data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let laccy = std::env::current_exe()?.with_file_name("laccy");
    let process = gio::Subprocess::newv(
        &[laccy.as_os_str(), OsStr::new("--thumbnail")],
        gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE,
    )?;
    let (png, _) = process.communicate_future(Some(&glib::Bytes::from(data))).await?;
    match png {
        Some(png) if process.is_successful() && !png.is_empty() => Ok(png.to_vec()),
        _ => Err(format!("{} could not make a thumbnail", laccy.display()).into()),
    }
}

#[cfg(target_env = "gnu")]
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> std::ffi::c_int;
}

/// A copied image passes through several buffers its size. glibc keeps freed
/// blocks that large for reuse, and SQLite keeps pages it wrote, which would
/// leave this long-running process tens of megabytes bigger. Hand them back.
fn release_memory(history: &History) {
    if let Err(err) = history.release_memory() {
        eprintln!("laccyd: cannot free the database cache: {err}");
    }
    #[cfg(target_env = "gnu")]
    // SAFETY: malloc_trim only returns free memory to the system.
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(test)]
mod tests {
    use gio::prelude::*;

    use super::*;

    #[test]
    fn parses_every_method() {
        let text = ("hi", "a.desktop").to_variant();
        assert_eq!(parse_call("AddText", &text), Some((Content::Text("hi".into()), Vec::new(), "a.desktop".into())));

        let formatted = glib::Variant::tuple_from_iter([
            "hi".to_variant(),
            glib::Variant::array_from_iter_with_type(
                glib::VariantTy::new("{say}").unwrap(),
                [glib::Variant::from_dict_entry(
                    &"text/html".to_variant(),
                    &glib::Variant::array_from_fixed_array(b"<b>hi</b>"),
                )],
            ),
            "c.desktop".to_variant(),
        ]);
        let html = Format { mime_type: "text/html".into(), data: b"<b>hi</b>".to_vec() };
        assert_eq!(
            parse_call("AddFormattedText", &formatted),
            Some((Content::Text("hi".into()), vec![html], "c.desktop".into()))
        );

        let files = (vec!["file:///a"], "").to_variant();
        assert_eq!(
            parse_call("AddFiles", &files),
            Some((Content::Files(vec!["file:///a".into()]), Vec::new(), String::new()))
        );

        let image = glib::Variant::tuple_from_iter([
            glib::Variant::array_from_fixed_array(&[1u8, 2, 3]),
            "image/png".to_variant(),
            "b.desktop".to_variant(),
        ]);
        let expected = Content::Image { mime_type: "image/png".into(), data: vec![1, 2, 3] };
        assert_eq!(parse_call("AddImage", &image), Some((expected, Vec::new(), "b.desktop".into())));

        assert_eq!(parse_call("AddImage", &text), None);
    }
}
