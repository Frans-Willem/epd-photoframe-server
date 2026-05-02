//! Snapshot testing helpers — compare a rendered [`Pixmap`] against a
//! checked-in PNG reference under `tests/snapshots/<name>.png`.
//!
//! Run with `UPDATE_SNAPSHOTS=1 cargo test` to regenerate the
//! references. On mismatch, the actual rendering is written next to
//! the reference as `<name>.actual.png` for visual diffing
//! (gitignored).

#![cfg(test)]

use tiny_skia::Pixmap;

/// Assert that `pm` matches the PNG snapshot at
/// `tests/snapshots/<name>.png`. Pass e.g. `"battery_indicator/icon_75"`
/// to land at `tests/snapshots/battery_indicator/icon_75.png`.
pub fn assert_matches(pm: &Pixmap, name: &str) {
    let path = path(name);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        write_png(pm, &path);
        return;
    }
    let expected = read_png_rgba(&path);
    let actual = straight_rgba(pm);
    if actual != expected {
        let actual_path = format!("{path}.actual.png");
        write_png(pm, &actual_path);
        panic!(
            "snapshot mismatch for `{name}`\n  expected: {path}\n  actual:   {actual_path}\n\
             rerun with `UPDATE_SNAPSHOTS=1 cargo test` to update"
        );
    }
}

fn path(name: &str) -> String {
    format!(
        "{}/tests/snapshots/{}.png",
        env!("CARGO_MANIFEST_DIR"),
        name
    )
}

/// Pixmap stores premultiplied RGBA. PNG encode/decode round-trips
/// through straight RGBA, so demultiply when reading the Pixmap to
/// compare apples-to-apples with what the PNG decoder hands us.
fn straight_rgba(pm: &Pixmap) -> Vec<u8> {
    pm.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

fn write_png(pm: &Pixmap, path: &str) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).expect("create snapshot dir");
    }
    let bytes = pm.encode_png().expect("encode PNG");
    std::fs::write(path, bytes).expect("write snapshot");
}

fn read_png_rgba(path: &str) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        panic!(
            "missing snapshot {path}: {e}\n\
             rerun with `UPDATE_SNAPSHOTS=1 cargo test` to generate"
        )
    });
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("read PNG header");
    let buf_size = reader.output_buffer_size().expect("PNG output buffer size");
    let mut buf = vec![0; buf_size];
    let info = reader.next_frame(&mut buf).expect("decode PNG frame");
    buf.truncate(info.buffer_size());
    buf
}
