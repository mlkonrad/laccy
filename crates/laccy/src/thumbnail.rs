//! `laccy --thumbnail` reads an image on stdin and writes a PNG thumbnail,
//! scaled down to fit a popup row, to stdout. laccyd runs it for every copied
//! image: the image libraries hold on to memory, so they're only loaded in a
//! process that exits right after.

use std::io::{Read, Write};

use gtk::gdk_pixbuf::{self, prelude::*};
use gtk::glib;

pub fn run() -> glib::ExitCode {
    let mut data = Vec::new();
    let result = std::io::stdin()
        .read_to_end(&mut data)
        .map_err(|err| err.to_string())
        .and_then(|_| thumbnail(&data).map_err(|err| err.to_string()))
        .and_then(|png| std::io::stdout().write_all(&png).map_err(|err| err.to_string()));
    match result {
        Ok(()) => glib::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("laccy: cannot make a thumbnail: {err}");
            glib::ExitCode::FAILURE
        }
    }
}

/// A PNG of the image in `data`, scaled down to fit a popup row.
fn thumbnail(data: &[u8]) -> Result<Vec<u8>, glib::Error> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.connect_size_prepared(|loader, width, height| {
        let (width, height) = laccy_core::thumbnail_size(width, height);
        loader.set_size(width, height);
    });
    // Close even after a failed write, or the loader warns when it's dropped.
    let written = loader.write(data);
    let closed = loader.close();
    written.and(closed)?;
    let pixbuf = loader
        .pixbuf()
        .ok_or_else(|| glib::Error::new(gdk_pixbuf::PixbufError::CorruptImage, "the image has no pixels"))?;
    pixbuf.save_to_bufferv("png", &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: i32, height: i32) -> Vec<u8> {
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, width, height).unwrap();
        pixbuf.fill(0x3366_99ff);
        pixbuf.save_to_bufferv("png", &[]).unwrap()
    }

    #[test]
    fn thumbnails_are_scaled_pngs() {
        let thumbnail = thumbnail(&png(1920, 1080)).unwrap();
        let loader = gdk_pixbuf::PixbufLoader::with_type("png").unwrap();
        loader.write(&thumbnail).unwrap();
        loader.close().unwrap();
        let pixbuf = loader.pixbuf().unwrap();
        assert_eq!((pixbuf.width(), pixbuf.height()), laccy_core::thumbnail_size(1920, 1080));
    }

    #[test]
    fn rejects_data_that_is_not_an_image() {
        assert!(thumbnail(b"not an image").is_err());
    }
}
