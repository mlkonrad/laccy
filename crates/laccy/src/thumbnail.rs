//! `laccy --thumbnail` reads an image on stdin and writes a PNG thumbnail,
//! scaled down to fit a popup row, to stdout. laccyd runs it for every copied
//! image: the image libraries hold on to memory, so they're only loaded in a
//! process that exits right after.

use std::io::{Read, Write};

use gtk::gdk_pixbuf::{self, prelude::*};
use gtk::glib;

/// `mime_type` is the image's format as the app that copied it said.
pub fn run(mime_type: Option<&str>) -> glib::ExitCode {
    let mut data = Vec::new();
    let result = std::io::stdin()
        .read_to_end(&mut data)
        .map_err(|err| err.to_string())
        .and_then(|_| thumbnail(&data, mime_type.unwrap_or_default()).map_err(|err| err.to_string()))
        .and_then(|png| std::io::stdout().write_all(&png).map_err(|err| err.to_string()));
    match result {
        Ok(()) => glib::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("laccy: cannot make a thumbnail: {err}");
            glib::ExitCode::FAILURE
        }
    }
}

/// Fed to the loader at a time, so it can stop once the image's size is known.
const CHUNK_BYTES: usize = 64 * 1024;

/// A PNG of the image in `data`, scaled down to fit a popup row. Only data
/// in a format from [`laccy_core::IMAGE_MIME_TYPES`], matching the
/// `mime_type` it was copied as, is decoded: loaders pick a decoder from the
/// data itself, which could be any of dozens.
fn thumbnail(data: &[u8], mime_type: &str) -> Result<Vec<u8>, glib::Error> {
    if !laccy_core::is_image_of_type(mime_type, data) {
        return Err(glib::Error::new(gdk_pixbuf::PixbufError::UnknownType, "not an image in a format Laccy records"));
    }
    let loader = gdk_pixbuf::PixbufLoader::with_mime_type(mime_type)?;
    let too_large = std::rc::Rc::new(std::cell::Cell::new(false));
    loader.connect_size_prepared({
        let too_large = std::rc::Rc::clone(&too_large);
        move |loader, width, height| {
            if i64::from(width) * i64::from(height) > laccy_core::MAX_IMAGE_PIXELS {
                too_large.set(true);
            }
            let (width, height) = laccy_core::thumbnail_size(width, height);
            loader.set_size(width, height);
        }
    });
    // Close even after a failed write, or the loader warns when it's dropped.
    let mut written = Ok(());
    for chunk in data.chunks(CHUNK_BYTES) {
        written = loader.write(chunk);
        if written.is_err() || too_large.get() {
            break;
        }
    }
    let closed = loader.close();
    if too_large.get() {
        return Err(glib::Error::new(gdk_pixbuf::PixbufError::InsufficientMemory, "the image has too many pixels"));
    }
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
        let thumbnail = thumbnail(&png(1920, 1080), "image/png").unwrap();
        let loader = gdk_pixbuf::PixbufLoader::with_type("png").unwrap();
        loader.write(&thumbnail).unwrap();
        loader.close().unwrap();
        let pixbuf = loader.pixbuf().unwrap();
        assert_eq!((pixbuf.width(), pixbuf.height()), laccy_core::thumbnail_size(1920, 1080));
    }

    #[test]
    fn rejects_data_that_is_not_an_image() {
        assert!(thumbnail(b"not an image", "image/png").is_err());
    }

    #[test]
    fn decodes_only_the_declared_known_format() {
        let xpm = b"/* XPM */\nstatic char *x[] = {\"1 1 1 1\", \"a c #000000\", \"a\"};\n";
        assert!(thumbnail(xpm, "image/x-xpixmap").is_err());
        // Labelled as a recorded format, it must not be sniffed and decoded anyway.
        assert!(thumbnail(xpm, "image/png").is_err());
        assert!(thumbnail(&png(4, 4), "").is_err());
    }

    /// A black 1-bit PNG of `size` x `size` pixels: like a decompression
    /// bomb, a small file that takes gigabytes to decode to RGBA.
    fn black_png(size: u32) -> Vec<u8> {
        use gtk::gio::{self, prelude::*};

        let chunk = |data: &mut Vec<u8>, kind: &[u8], body: &[u8]| {
            let typed = [kind, body].concat();
            data.extend(u32::try_from(body.len()).unwrap().to_be_bytes());
            data.extend(&typed);
            data.extend(crc32(&typed).to_be_bytes());
        };
        let compressed = gio::MemoryOutputStream::new_resizable();
        let zlib = gio::ConverterOutputStream::new(&compressed, &gio::ZlibCompressor::new(gio::ZlibCompressorFormat::Zlib, 9));
        // A filter byte, then one bit per pixel.
        let row = vec![0u8; 1 + size.div_ceil(8) as usize];
        for _ in 0..size {
            zlib.write_all(&row, gio::Cancellable::NONE).unwrap();
        }
        zlib.close(gio::Cancellable::NONE).unwrap();

        let mut data = b"\x89PNG\r\n\x1a\n".to_vec();
        chunk(&mut data, b"IHDR", &[&size.to_be_bytes()[..], &size.to_be_bytes(), &[1, 0, 0, 0, 0]].concat());
        chunk(&mut data, b"IDAT", &compressed.steal_as_bytes());
        chunk(&mut data, b"IEND", b"");
        data
    }

    fn crc32(bytes: &[u8]) -> u32 {
        !bytes.iter().fold(!0u32, |crc, &byte| {
            (0..8).fold(crc ^ u32::from(byte), |crc, _| if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 })
        })
    }

    #[test]
    fn rejects_images_with_too_many_pixels() {
        // 144 megapixels.
        let bomb = black_png(12_000);
        assert!(bomb.len() < 1024 * 1024);
        let err = thumbnail(&bomb, "image/png").unwrap_err();
        assert!(err.message().contains("too many pixels"), "{err}");
        // Just under the limit is fine.
        assert!(thumbnail(&black_png(9_000), "image/png").is_ok());
    }
}
