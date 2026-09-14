//! Copied files, which the clipboard carries as a list of `file://` URIs.

use std::path::PathBuf;

/// The clipboard format of copied files.
pub const MIME_TYPE: &str = "text/uri-list";

/// Serializes URIs as a `text/uri-list`, whose lines end in CRLF.
pub fn uri_list<S: AsRef<str>>(uris: &[S]) -> String {
    uris.iter().map(|uri| format!("{}\r\n", uri.as_ref())).collect()
}

/// The local path of a `file://` URI, or `None` for any other URI.
pub fn path(uri: &str) -> Option<PathBuf> {
    gio::glib::filename_from_uri(uri).ok().map(|(path, _host)| path)
}

/// The path of `uri` for showing and searching, or the URI itself if it has none.
pub fn display_path(uri: &str) -> String {
    path(uri).map_or_else(|| uri.to_owned(), |path| path.to_string_lossy().into_owned())
}

/// The file name of `uri`, like `report.pdf`.
pub fn display_name(uri: &str) -> String {
    match path(uri) {
        Some(path) => path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
            .into_owned(),
        None => uri.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_list_ends_lines_in_crlf() {
        assert_eq!(uri_list(&["file:///a", "file:///b"]), "file:///a\r\nfile:///b\r\n");
    }

    #[test]
    fn decodes_file_uris() {
        assert_eq!(path("file:///home/me/My%20Report.pdf"), Some(PathBuf::from("/home/me/My Report.pdf")));
        assert_eq!(display_name("file:///home/me/My%20Report.pdf"), "My Report.pdf");
        assert_eq!(display_name("file:///"), "/");
        assert_eq!(path("https://example.com/a.pdf"), None);
        assert_eq!(display_path("https://example.com/a.pdf"), "https://example.com/a.pdf");
    }
}
