//! Bounded still-image conversion; publication keeps the original until its
//! replacement manifest has been committed by the collection worker.
use crate::Result;
use image::{AnimationDecoder, ColorType, DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use searchcord_archive::media;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read, Write},
    path::Path,
};

const MAX_PIXELS: u64 = 32_000_000;

fn cmyk_jpeg(bytes: &[u8]) -> bool {
    let mut at = 2;
    while at + 4 <= bytes.len() && bytes[at] == 0xff {
        while at < bytes.len() && bytes[at] == 0xff {
            at += 1;
        }
        let Some(&marker) = bytes.get(at) else {
            break;
        };
        at += 1;
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        let Some(length) = bytes.get(at..at + 2) else {
            break;
        };
        let length = u16::from_be_bytes([length[0], length[1]]) as usize;
        if length < 2 || at + length > bytes.len() {
            break;
        }
        if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
            return length >= 8 && bytes[at + 7] == 4;
        }
        at += length;
    }
    false
}

fn encoded(bytes: &[u8], kind: &str) -> Result<Option<Vec<u8>>> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let format = reader.format();
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > MAX_PIXELS
        || width == 0
        || height == 0
        || !matches!(
            decoder.color_type(),
            ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
        )
        || (format == Some(ImageFormat::Jpeg) && cmyk_jpeg(bytes))
    {
        return Ok(None);
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_PIXELS * 4);
    decoder.set_limits(limits.clone())?;
    let animated = match format {
        Some(ImageFormat::Png) => {
            image::codecs::png::PngDecoder::new(Cursor::new(bytes))?.is_apng()?
        }
        Some(ImageFormat::WebP) => {
            image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))?.has_animation()
        }
        Some(ImageFormat::Gif) => {
            let mut gif = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
            gif.set_limits(limits)?;
            gif.into_frames()
                .take(2)
                .collect::<image::ImageResult<Vec<_>>>()?
                .len()
                > 1
        }
        _ => false,
    };
    if animated {
        return Ok(None);
    }
    let orientation = decoder.orientation()?;
    let icc = decoder.icc_profile()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    let edge = match kind {
        "avatar" => 128,
        "banner" => 1024,
        _ => 1600,
    };
    if image.width() > edge || image.height() > edge {
        image = image.resize(edge, edge, image::imageops::FilterType::Lanczos3);
    }
    let pixels = image.to_rgba8();
    let mut config = webp::WebPConfig::new().map_err(|_| "Could not initialize WebP encoder")?;
    config.quality = 80.0;
    config.method = 4;
    let encoded = webp::Encoder::from_rgba(pixels.as_raw(), pixels.width(), pixels.height())
        .encode_advanced(&config)
        .map_err(|_| "WebP encoding failed")?;
    let mut output = encoded.to_vec();
    if let Some(icc) = icc.filter(|icc| !icc.is_empty()) {
        // libwebp supplies the image chunks. Add the ICC profile in the WebP
        // extended container, before the image data, without EXIF/XMP metadata.
        let mut extended = output[..12].to_vec();
        let remainder = if output.get(12..16) == Some(b"VP8X") {
            extended.extend_from_slice(&output[12..30]);
            extended[20] |= 0x20;
            &output[30..]
        } else {
            extended.extend_from_slice(b"VP8X\x0a\0\0\0\x20\0\0\0");
            extended.extend_from_slice(&(pixels.width() - 1).to_le_bytes()[..3]);
            extended.extend_from_slice(&(pixels.height() - 1).to_le_bytes()[..3]);
            &output[12..]
        };
        extended.extend_from_slice(b"ICCP");
        extended.extend_from_slice(&(icc.len() as u32).to_le_bytes());
        extended.extend_from_slice(&icc);
        if icc.len() % 2 == 1 {
            extended.push(0);
        }
        extended.extend_from_slice(remainder);
        let length = (extended.len() - 8) as u32;
        extended[4..8].copy_from_slice(&length.to_le_bytes());
        output = extended;
    }
    // Validate the completed container before making it eligible for publication.
    image::load_from_memory_with_format(&output, ImageFormat::WebP)?;
    Ok(Some(output))
}

pub(crate) fn optimize(directory: &Path, key: &str, data: &Value) -> Result<Option<Value>> {
    if data.get("storage_file").is_some()
        || data["bytes"].as_u64().unwrap_or(u64::MAX) > super::media_collection::MAX_BYTES
        || !data["content_type"].as_str().is_some_and(|kind| {
            [
                "image/png",
                "image/jpeg",
                "image/gif",
                "image/webp",
                "image/avif",
            ]
            .contains(&kind)
        })
    {
        return Ok(None);
    }
    let original = media::binary_path(directory, key, data)?;
    if super::media_collection::symlink(directory)
        || super::media_collection::symlink(&original)
        || data["bytes"].as_u64() != Some(fs::metadata(&original)?.len())
    {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    fs::File::open(&original)?
        .take(super::media_collection::MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if data["bytes"].as_u64() != Some(bytes.len() as u64)
        || data["sha256"].as_str() != Some(&format!("{:x}", Sha256::digest(&bytes)))
    {
        return Ok(None);
    }
    let kind = media::media_kind(data["source_url"].as_str().unwrap_or(""));
    let Some(encoded) = encoded(&bytes, kind)? else {
        return Ok(None);
    };
    if encoded.len() >= bytes.len() {
        return Ok(None);
    }
    let digest = format!("{:x}", Sha256::digest(&encoded));
    let filename = format!("{key}.{digest}.webp");
    let mut temporary = tempfile::Builder::new()
        .prefix(".searchcord-media-")
        .suffix(".tmp")
        .tempfile_in(directory)?;
    temporary.write_all(&encoded)?;
    temporary.flush()?;
    temporary
        .persist(directory.join(&filename))
        .map_err(|error| error.error)?;
    let mut compact = data.clone();
    compact["storage_file"] = json!(filename);
    compact["original_bytes"] = data["bytes"].clone();
    compact["original_sha256"] = data["sha256"].clone();
    compact["original_content_type"] = data["content_type"].clone();
    compact["content_type"] = json!("image/webp");
    compact["bytes"] = json!(encoded.len());
    compact["sha256"] = json!(digest);
    Ok(Some(compact))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageEncoder, Rgba, RgbaImage};

    fn png() -> Vec<u8> {
        let pixels = RgbaImage::from_fn(512, 256, |x, y| {
            Rgba([x as u8, y as u8, (x * y) as u8, 128])
        });
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(pixels)
            .write_to(&mut output, ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    #[test]
    fn prepared_copy_retains_original_and_rejects_unapproved_types_or_damage() {
        let directory = tempfile::tempdir().unwrap();
        let url = "https://cdn.discordapp.com/avatars/30/hash.png";
        let key = media::media_key(url).unwrap();
        let bytes = png();
        let binary = directory.path().join(format!("{key}.bin"));
        fs::write(&binary, &bytes).unwrap();
        let mut data = json!({"status":"saved","source_url":url,"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"content_type":"image/jpg"});
        assert!(optimize(directory.path(), &key, &data).unwrap().is_none());
        data["content_type"] = json!("image/png");
        let compact = optimize(directory.path(), &key, &data).unwrap().unwrap();
        assert!(compact["bytes"].as_u64().unwrap() < bytes.len() as u64);
        assert!(
            media::binary_path(directory.path(), &key, &compact)
                .unwrap()
                .is_file()
        );
        assert_eq!(fs::read(&binary).unwrap(), bytes);
        assert!(!directory.path().join(format!("{key}.json")).exists());
        data["sha256"] = json!("0".repeat(64));
        assert!(optimize(directory.path(), &key, &data).unwrap().is_none());
        assert!(binary.is_file());
    }

    #[test]
    fn compact_collection_is_qualified_in_a_separate_process() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "media_compact::tests::compact_collection_worker",
                "--nocapture",
            ])
            .env("SEARCHCORD_COMPACT_TEST", "1")
            .env("SEARCHCORD_MEDIA_MODE", "compact")
            .env("SEARCHCORD_PROFILE_IMAGES", "original")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn compact_collection_worker() {
        if std::env::var("SEARCHCORD_COMPACT_TEST").is_err() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.db");
        fs::write(&path, b"fixture").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let bytes = png();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            write!(socket,"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",bytes.len()).unwrap();
            socket.write_all(&bytes).unwrap();
        });
        let collector = crate::media_collection::MediaCollector::for_loopback(
            &path,
            &origin,
            Default::default(),
        )
        .unwrap();
        let url = "https://cdn.discordapp.com/avatars/30/hash.png";
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(collector.cache_media(url, None))
            .unwrap();
        server.join().unwrap();
        assert_eq!(result["status"], "saved");
        assert!(result["storage_file"].is_string());
        let key = media::media_key(url).unwrap();
        assert!(media::verified_file(&path, &key).unwrap().path.is_file());
        assert!(!media::directory(&path).join(format!("{key}.bin")).exists());
    }

    #[test]
    fn compact_preserves_alpha_orientation_and_icc_without_upscaling() {
        let pixels = RgbaImage::from_fn(512, 1024, |x, y| {
            Rgba([x as u8, y as u8, (x * y) as u8, 128])
        });
        let mut input = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut input);
        let profile = b"synthetic ICC profile".to_vec();
        encoder.set_icc_profile(profile.clone()).unwrap();
        // Little-endian TIFF orientation 6 rotates the source 90 degrees.
        encoder
            .set_exif_metadata(
                b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec(),
            )
            .unwrap();
        encoder
            .write_image(pixels.as_raw(), 512, 1024, image::ExtendedColorType::Rgba8)
            .unwrap();
        let output = encoded(&input, "avatar").unwrap().unwrap();
        let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&output)).unwrap();
        assert_eq!(decoder.dimensions(), (128, 64));
        assert_eq!(decoder.icc_profile().unwrap(), Some(profile));
        assert!(decoder.exif_metadata().unwrap().is_none());
        let result = DynamicImage::from_decoder(decoder).unwrap().to_rgba8();
        assert!(result.pixels().all(|pixel| pixel[3] == 128));
        let mut small = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(12, 6)
            .write_to(&mut small, ImageFormat::Png)
            .unwrap();
        let output = encoded(small.get_ref(), "avatar").unwrap().unwrap();
        assert_eq!(image::load_from_memory(&output).unwrap().width(), 12);
    }

    #[test]
    fn animated_gif_is_retained() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for value in [0, 255] {
                encoder
                    .encode_frame(image::Frame::new(RgbaImage::from_pixel(
                        10,
                        10,
                        Rgba([value, 0, 0, 255]),
                    )))
                    .unwrap();
            }
        }
        assert!(encoded(&bytes, "avatar").unwrap().is_none());
    }
}
