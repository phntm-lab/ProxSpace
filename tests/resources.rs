//! The Windows resources `build.rs` embeds into `proxspace.exe`.
//!
//! They are compiled by `rc.exe`, outside anything the rest of the tests can
//! see, and a broken icon or a manifest with a typo in it shows up only as a
//! silent `cargo:warning` and a plain executable. These checks read the two
//! files the way the resource compiler will.

const ICON: &[u8] = include_bytes!("../assets/proxspace.ico");
const MANIFEST: &str = include_str!("../assets/proxspace.manifest");

/// One entry of the icon directory: what size it is and where its bitmap lives.
struct Entry {
    width: u32,
    height: u32,
    bits_per_pixel: u16,
    size: usize,
    offset: usize,
}

fn entries() -> Vec<Entry> {
    let word = |at: usize| u16::from_le_bytes([ICON[at], ICON[at + 1]]);
    let long = |at: usize| {
        u32::from_le_bytes([ICON[at], ICON[at + 1], ICON[at + 2], ICON[at + 3]]) as usize
    };

    assert_eq!(word(0), 0, "not an icon directory");
    assert_eq!(word(2), 1, "type 1 is an icon; 2 would be a cursor");
    let count = word(4) as usize;
    assert!(count > 0, "the icon is empty");

    (0..count)
        .map(|index| {
            let at = 6 + 16 * index;
            Entry {
                // 0 in a directory entry means 256: the field is one byte.
                width: if ICON[at] == 0 { 256 } else { ICON[at] as u32 },
                height: if ICON[at + 1] == 0 {
                    256
                } else {
                    ICON[at + 1] as u32
                },
                bits_per_pixel: word(at + 6),
                size: long(at + 8),
                offset: long(at + 12),
            }
        })
        .collect()
}

#[test]
fn the_icon_carries_every_size_windows_asks_for() {
    let sizes: Vec<u32> = entries().iter().map(|entry| entry.width).collect();

    // 16 is the title bar and the small view, 32 the desktop, 256 the preview
    // pane; the ones between are what Explorer scales from at other DPIs.
    for expected in [16, 32, 48, 256] {
        assert!(
            sizes.contains(&expected),
            "no {expected}px image: {sizes:?}"
        );
    }
}

#[test]
fn every_image_is_square_true_colour_and_inside_the_file() {
    for entry in entries() {
        assert_eq!(
            entry.width, entry.height,
            "{}px image is not square",
            entry.width
        );
        assert_eq!(
            entry.bits_per_pixel, 32,
            "{}px image is not 32-bit; the mark has soft edges and needs the alpha",
            entry.width
        );
        assert!(
            entry.offset + entry.size <= ICON.len(),
            "{}px image runs past the end of the file",
            entry.width
        );

        let image = &ICON[entry.offset..entry.offset + entry.size];
        let png = image.starts_with(b"\x89PNG\r\n\x1a\n");
        if png {
            // Only the largest image is worth compressing, and only Vista and
            // later read PNG entries at all.
            assert_eq!(entry.width, 256, "a PNG entry at {}px", entry.width);
        } else {
            // A DIB inside an icon carries the AND mask below the pixels, so
            // its header says twice the height it really is.
            let header_height = i32::from_le_bytes([image[8], image[9], image[10], image[11]]);
            assert_eq!(
                header_height,
                entry.height as i32 * 2,
                "{}px DIB has no mask height",
                entry.width
            );
        }
    }
}

#[test]
fn the_manifest_asks_for_nothing_it_does_not_need() {
    assert!(MANIFEST.starts_with("<?xml"));
    assert!(MANIFEST.contains(r#"<requestedExecutionLevel level="asInvoker" uiAccess="false"/>"#));
    assert!(MANIFEST.contains("<longPathAware"));

    // Elevation is the whole point of the manifest: ProxSpace writes next to
    // its own executable and must never ask for administrator rights.
    for forbidden in [
        "requireAdministrator",
        "highestAvailable",
        "uiAccess=\"true\"",
    ] {
        assert!(
            !MANIFEST.contains(forbidden),
            "the manifest asks for `{forbidden}`"
        );
    }
}

/// Both tags are opened once and closed once. Not a parser — just enough to
/// catch the edit that leaves `rc.exe` with an unbalanced file.
#[test]
fn the_manifest_is_a_single_well_formed_assembly() {
    for tag in ["assembly", "trustInfo", "application", "compatibility"] {
        assert_eq!(
            MANIFEST.matches(&format!("<{tag}")).count(),
            MANIFEST.matches(&format!("</{tag}>")).count(),
            "`{tag}` is not balanced"
        );
    }
    assert_eq!(MANIFEST.matches("<assembly").count(), 1);
}
