//! laccyd records clipboard changes reported by the GNOME Shell extension.
//! D-Bus activation starts it on the first copy. It has no UI, so it stays small.

use std::cell::RefCell;
use std::rc::Rc;

use gio::glib;
use laccy_core::config::{self, Config};
use laccy_core::history::History;
use laccy_core::{DAEMON_BUS_NAME, DAEMON_INTERFACE, DAEMON_OBJECT_PATH};

const INTROSPECTION: &str = r#"
<node>
  <interface name="io.github.mlkonrad.Laccy.Daemon1">
    <method name="AddText">
      <arg name="text" type="s" direction="in"/>
      <arg name="source_app" type="s" direction="in"/>
    </method>
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
        .method_call(move |_, _, _, _, method, params, invocation| match method {
            "AddText" => {
                let Some((text, source_app)) = params.get::<(String, String)>() else {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "expected (ss)");
                    return;
                };
                let config = settings.as_ref().map_or_else(Config::default, Config::from_settings);
                match add_text(&mut history.borrow_mut(), &config, &text, &source_app) {
                    Ok(()) => invocation.return_value(None),
                    Err(err) => {
                        eprintln!("laccyd: cannot store clipboard item: {err}");
                        invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &err.to_string());
                    }
                }
            }
            _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
        })
        .build();

    if let Err(err) = registration {
        eprintln!("laccyd: cannot export {DAEMON_OBJECT_PATH}: {err}");
        std::process::exit(1);
    }
}

fn add_text(
    history: &mut History,
    config: &Config,
    text: &str,
    source_app: &str,
) -> laccy_core::history::Result<()> {
    if text.trim().is_empty() || text.len() > config.max_item_bytes || config.is_ignored(source_app) {
        return Ok(());
    }
    let source_app = (!source_app.is_empty()).then_some(source_app);
    history.add(text, source_app, config.history_size)?;
    Ok(())
}
