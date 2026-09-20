//! Images a turn can show the model.
//!
//! Two ways in, one shape out. A path the user typed, pasted or dragged into the
//! terminal (`"C:\Users\me\shot.png"`) is found in the message text; a screenshot that
//! only exists as a bitmap on the clipboard is read by the app itself, because **no
//! terminal sends a clipboard bitmap as text** — bracketed paste carries characters or
//! nothing. Both become the same attachment, and both are bounded the same way.
//!
//! The API is what sets the rules this module enforces: PNG, JPEG, GIF or WebP,
//! detected from the **bytes** rather than from a file name or a declared type, at most
//! 32 MiB per image and 48 MiB per request body. An attachment is content the user
//! explicitly named, so it is not confined to the workspace the way a tool path is —
//! but it is still refused when the path names a place where credentials live, and a
//! file that is not really an image is refused with the reason instead of being sent.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use harness_providers::ImageAttachment;

/// Longest side and byte size the API accepts, with room for the base64 expansion.
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Most images one turn may carry: three are already ~32 MiB of base64 in the body.
pub const MAX_IMAGES: usize = 3;

/// One image ready to be attached to a message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedImage {
    pub attachment: ImageAttachment,
    /// Where it came from, for the transcript.
    pub source: String,
}

/// The images one message names, plus the reasons any candidate was left out.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Attachments {
    pub images: Vec<PreparedImage>,
    /// One line per candidate that was recognised but not attached.
    pub notes: Vec<String>,
}

impl Attachments {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

/// Read every image path one user message names.
///
/// A path with spaces has to be quoted, exactly as a shell would require; a bare word
/// is still accepted, which is how a drag-and-drop lands in most terminals.
#[must_use]
pub fn from_message(text: &str, workspace: &Path) -> Attachments {
    let mut result = Attachments::default();
    for candidate in candidate_paths(text) {
        if result.images.len() >= MAX_IMAGES {
            result.notes.push(format!(
                "only the first {MAX_IMAGES} images are attached; {candidate} was left out"
            ));
            break;
        }
        let Some(path) = resolve(&candidate, workspace) else {
            continue;
        };
        match read_image_file(&path) {
            Ok(image) => {
                if !result
                    .images
                    .iter()
                    .any(|existing| existing.source == image.source)
                {
                    result.images.push(image);
                }
            }
            Err(reason) => result.notes.push(reason),
        }
    }
    result
}

/// Render a byte count the way a reader expects it.
///
/// A small screenshot must not be reported as `0 KiB`: the transcript would say an
/// image arrived and that it is empty in the same breath. Integer arithmetic keeps
/// the value exact — no rounding can make two different files look the same size —
/// and a tenth is dropped when it is zero, so a round size reads as `84 KiB`.
fn human_size(bytes: u64) -> String {
    /// `bytes` in units of `unit`, with one decimal only when it says something.
    fn scaled(bytes: u64, unit: u64, name: &str) -> String {
        let tenth = (bytes % unit) * 10 / unit;
        if tenth == 0 {
            format!("{} {name}", bytes / unit)
        } else {
            format!("{}.{tenth} {name}", bytes / unit)
        }
    }

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < MIB {
        scaled(bytes, KIB, "KiB")
    } else {
        scaled(bytes, MIB, "MiB")
    }
}

/// Turn one image file into an attachment, or explain why it cannot be shown.
pub fn read_image_file(path: &Path) -> Result<PreparedImage, String> {
    let name = path.display().to_string();
    let metadata = std::fs::metadata(path).map_err(|error| format!("{name}: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{name}: not a file"));
    }
    if metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err(format!(
            "{name}: {} is larger than the {} MiB an image may be",
            human_size(metadata.len()),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("{name}: {error}"))?;
    attachment_from_bytes(&name, &bytes, &metadata)
}

/// Build the attachment from bytes that were read from `source`.
fn attachment_from_bytes(
    label_source: &str,
    bytes: &[u8],
    metadata: &std::fs::Metadata,
) -> Result<PreparedImage, String> {
    let Some(media_type) = sniff(bytes) else {
        return Err(format!(
            "{label_source}: not a PNG, JPEG, GIF or WebP (the API detects the format from the bytes)"
        ));
    };
    if looks_like_credentials(label_source) {
        return Err(format!(
            "{label_source}: a credential file is never attached to a request"
        ));
    }
    let label = format!(
        "{} ({media_type}, {})",
        Path::new(label_source).file_name().map_or_else(
            || label_source.to_owned(),
            |name| name.to_string_lossy().into_owned()
        ),
        human_size(metadata.len())
    );
    Ok(PreparedImage {
        attachment: ImageAttachment {
            media_type: media_type.to_owned(),
            data_base64: BASE64.encode(bytes),
            label,
        },
        source: label_source.to_owned(),
    })
}

/// The media type of an image, decided by the bytes the way the API decides it.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.starts_with(PNG) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Take a bitmap off the clipboard and encode it as PNG.
///
/// `Ok(None)` means the clipboard holds no image, which is the ordinary case when
/// someone pastes text. The RGBA the clipboard hands back is encoded here because the
/// API reads images only as PNG, JPEG, GIF or WebP.
pub fn clipboard_png() -> Result<Option<Vec<u8>>, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|error| format!("clipboard: {error}"))?;
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(error) => return Err(format!("clipboard: {error}")),
    };
    if image.width == 0 || image.height == 0 {
        return Ok(None);
    }
    let width = u32::try_from(image.width).map_err(|_| "clipboard image is too wide".to_owned())?;
    let height =
        u32::try_from(image.height).map_err(|_| "clipboard image is too tall".to_owned())?;
    let mut png = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut png),
        &image.bytes,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|error| format!("clipboard image could not be encoded: {error}"))?;
    if png.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "the pasted image is {} MiB; the limit is {} MiB",
            png.len() / (1024 * 1024),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    Ok(Some(png))
}

/// Write a pasted PNG somewhere the message can name it.
///
/// The path is what the composer receives, so the ordinary path detection attaches it:
/// one code path for a screenshot, a drag-and-drop and a typed path.
pub fn save_pasted_png(directory: &Path, png: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let digest = harness_types::ContentHash::from_bytes(png);
    let stem = digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(digest.as_str());
    let path = directory.join(format!("paste-{}.png", &stem[..16.min(stem.len())]));
    std::fs::write(&path, png).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

/// Candidate path tokens in one message, longest-declared first.
///
/// Quoted spans are taken whole so a path with spaces survives; the rest of the text is
/// split on whitespace, because that is what a drop into a terminal produces.
fn candidate_paths(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut rest = String::with_capacity(text.len());
    let mut quoted = None::<char>;
    let mut current = String::new();
    for character in text.chars() {
        match quoted {
            Some(terminator) => {
                if character == terminator {
                    quoted = None;
                    if !current.is_empty() {
                        candidates.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(character);
                }
            }
            None => match character {
                '"' | '\'' => {
                    quoted = Some(character);
                    current.clear();
                }
                _ => rest.push(character),
            },
        }
    }
    // An unterminated quote still holds a usable path.
    if !current.is_empty() {
        candidates.push(current);
    }
    for word in rest.split_whitespace() {
        let trimmed = word.trim_matches(|character: char| {
            matches!(
                character,
                ',' | ';' | ':' | ')' | '(' | '[' | ']' | '<' | '>'
            )
        });
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_owned());
        }
    }
    candidates
}

/// Resolve a token the way a user means it: absolute, or relative to the workspace.
fn resolve(token: &str, workspace: &Path) -> Option<PathBuf> {
    let trimmed = token.trim().trim_matches('"').trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let in_workspace = workspace.join(path);
        if in_workspace.is_file() {
            in_workspace
        } else {
            path.to_path_buf()
        }
    };
    candidate.is_file().then_some(candidate)
}

/// Whether a path names a place where credentials live.
///
/// An attachment is content the user named, so it is not restricted to the workspace —
/// but a private key or a saved credential file is never sent to a provider, whatever
/// its bytes claim to be.
fn looks_like_credentials(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    let file_name = lowered
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(lowered.as_str());
    // A name is suspicious when any of its dotted segments is a credential kind.
    // `split` rather than `Path::extension` because a dotfile has no extension:
    // `.env` and `.env.local` are both a stem with a leading dot.
    let credential_segment = file_name
        .split('.')
        .skip(1)
        .any(|segment| matches!(segment, "pem" | "key" | "env"));
    lowered.split(['/', '\\']).any(|part| {
        matches!(
            part,
            ".ssh" | ".aws" | ".gnupg" | ".azure" | ".kube" | "credentials"
        )
    }) || file_name.starts_with("id_rsa")
        || file_name.starts_with("id_ed25519")
        || file_name.starts_with("credentials")
        || credential_segment
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_IMAGE_BYTES, MAX_IMAGES, clipboard_png, from_message, human_size,
        looks_like_credentials, read_image_file, save_pasted_png, sniff,
    };

    /// The smallest real PNG: 1x1, transparent.
    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, b'I', b'D', b'A', b'T', 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, b'I',
        b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82,
    ];

    fn write(directory: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, bytes).expect("fixture write");
        path
    }

    #[test]
    fn the_format_comes_from_the_bytes_not_the_name() {
        assert_eq!(sniff(PNG), Some("image/png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"not an image at all"), None);

        let temp = tempfile::tempdir().expect("temp root");
        let liar = write(temp.path(), "shot.png", b"just text, honestly");
        let error = read_image_file(&liar).expect_err("a text file named .png is refused");
        assert!(error.contains("not a PNG"), "{error}");
    }

    #[test]
    fn a_small_image_is_not_reported_as_empty() {
        // The turn summary and the transcript both quote this label, so a 165-byte
        // screenshot must not read as `0 KiB`.
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(165), "165 B");
        assert_eq!(human_size(1024), "1 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(84 * 1024), "84 KiB");
        assert_eq!(human_size(1024 * 1024), "1 MiB");
        assert_eq!(human_size(MAX_IMAGE_BYTES as u64), "8 MiB");

        let temp = tempfile::tempdir().expect("temp root");
        let small = write(temp.path(), "shot.png", PNG);
        let image = read_image_file(&small).expect("a real PNG is attached");
        assert_eq!(
            image.attachment.label,
            format!("shot.png (image/png, {} B)", PNG.len())
        );
        assert_eq!(image.source, small.display().to_string());
    }

    #[test]
    fn a_quoted_path_with_spaces_is_found_and_a_bare_one_is_too() {
        let temp = tempfile::tempdir().expect("temp root");
        let nested = temp.path().join("my pictures");
        std::fs::create_dir_all(&nested).expect("nested");
        let spaced = write(&nested, "shot.png", PNG);
        let bare = write(temp.path(), "other.png", PNG);

        let quoted = format!("look at \"{}\"", spaced.display());
        let found = from_message(&quoted, temp.path());
        assert_eq!(found.images.len(), 1, "{found:?}");
        assert!(found.notes.is_empty(), "{found:?}");
        assert_eq!(found.images[0].attachment.media_type, "image/png");
        assert!(found.images[0].attachment.data_base64.starts_with("iVBOR"));

        let found = from_message(&format!("see {}", bare.display()), temp.path());
        assert_eq!(found.images.len(), 1, "{found:?}");

        // A relative path resolves against the workspace.
        let relative = from_message("look at other.png", temp.path());
        assert_eq!(relative.images.len(), 1, "{relative:?}");
    }

    #[test]
    fn the_bounds_are_enforced_with_a_reason() {
        let temp = tempfile::tempdir().expect("temp root");
        for index in 0..=MAX_IMAGES {
            write(temp.path(), &format!("shot{index}.png"), PNG);
        }
        let message = (0..=MAX_IMAGES)
            .map(|index| format!("\"shot{index}.png\""))
            .collect::<Vec<_>>()
            .join(" ");
        let found = from_message(&message, temp.path());
        assert_eq!(found.images.len(), MAX_IMAGES, "{found:?}");
        assert!(
            found
                .notes
                .iter()
                .any(|note| note.contains("only the first")),
            "{found:?}"
        );

        let oversized = temp.path().join("huge.png");
        let mut bytes = PNG.to_vec();
        bytes.resize(MAX_IMAGE_BYTES + 1, 0);
        std::fs::write(&oversized, &bytes).expect("fixture write");
        let too_big = from_message(&format!("\"{}\"", oversized.display()), temp.path());
        assert!(too_big.images.is_empty());
        assert!(
            too_big
                .notes
                .iter()
                .any(|note| note.contains("larger than")),
            "{too_big:?}"
        );
    }

    #[test]
    fn a_credential_file_is_never_attached() {
        let temp = tempfile::tempdir().expect("temp root");
        let ssh = temp.path().join(".ssh");
        std::fs::create_dir_all(&ssh).expect("ssh dir");
        let key = write(&ssh, "id_rsa.png", PNG);
        let found = from_message(&format!("\"{}\"", key.display()), temp.path());
        assert!(found.images.is_empty(), "{found:?}");
        assert!(
            found
                .notes
                .iter()
                .any(|note| note.contains("credential file")),
            "{found:?}"
        );
        assert!(looks_like_credentials("C:/work/.env"));
        assert!(looks_like_credentials("C:/work/.env.local"));
        assert!(looks_like_credentials(".env"));
        assert!(looks_like_credentials("C:/keys/server.pem"));
        assert!(looks_like_credentials("C:/keys/deploy.key"));
        assert!(!looks_like_credentials("C:/work/shot.png"));
        assert!(!looks_like_credentials("C:/work/environment.png"));
    }

    #[test]
    fn a_saved_paste_is_named_by_its_content() {
        let temp = tempfile::tempdir().expect("temp root");
        let first = save_pasted_png(temp.path(), PNG).expect("saved");
        let second = save_pasted_png(temp.path(), PNG).expect("saved again");
        assert_eq!(first, second, "the same bytes land in the same file");
        assert!(first.is_file());

        let found = from_message(&format!("\"{}\"", first.display()), temp.path());
        assert_eq!(found.images.len(), 1, "{found:?}");
    }

    /// The clipboard is machine state, so this checks the shape of the answer rather
    /// than its content: no image must be an `Ok(None)`, never a hard failure.
    #[test]
    fn a_clipboard_without_an_image_is_not_an_error() {
        match clipboard_png() {
            Ok(None) => {}
            Ok(Some(png)) => assert_eq!(sniff(&png), Some("image/png")),
            Err(reason) => assert!(
                reason.starts_with("clipboard:"),
                "a real failure names the clipboard: {reason}"
            ),
        }
    }
}
