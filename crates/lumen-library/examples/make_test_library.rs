//! Writes a tagged test music folder (Hindi, Malayalam, Spanish and English
//! titles, embedded covers, `.lrc` files):
//!
//! ```text
//! cargo run -p lumen-library --example make_test_library -- <OUT_DIR> [ALBUMS]
//! ```

use std::path::Path;

use flacenc::component::BitRepr;
use flacenc::error::Verify;
use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::Tag;

const TITLES: &[&str] = &[
    "तुम ही हो",
    "अजनबी",
    "चाँद की रोशनी",
    "सपनों का शहर",
    "നിലാവ്",
    "മഴ",
    "അമ്മ",
    "പൂമുഖം",
    "Corazón",
    "Ñandú",
    "Árbol de la vida",
    "Canción del mar",
    "Night Drive",
    "Afterglow",
    "Paper Moon",
    "Track 10",
    "Track 2",
    "(I'm) Still Here",
];
const ARTISTS: &[&str] = &[
    "अरिजीत",
    "श्रेया",
    "ചിത്ര",
    "യേശുദാസ്",
    "Rosalía",
    "Los Ñandúes",
    "The Lumens",
    "Aurora Fields",
];

fn flac(frames: usize, seed: i32) -> Vec<u8> {
    let samples: Vec<i32> = (0..frames * 2)
        .map(|i| (((i as i32).wrapping_mul(7919) ^ seed) % 4000) - 2000)
        .collect();
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .expect("config");
    let source = flacenc::source::MemSource::from_samples(&samples, 2, 16, 44_100);
    let stream =
        flacenc::encode_with_fixed_block_size(&config, source, config.block_size).expect("encode");
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).expect("write");
    let mut b = sink.as_slice().to_vec();
    let max = [b[10], b[11]];
    b[8..10].copy_from_slice(&max); // libFLAC min/max block convention
    b
}

fn cover(seed: u8) -> Vec<u8> {
    let img = image::RgbImage::from_fn(600, 600, |x, y| {
        image::Rgb([(x / 3) as u8 ^ seed, (y / 3) as u8, seed.wrapping_mul(3)])
    });
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88)
        .encode_image(&img)
        .expect("jpeg");
    out
}

fn tag(path: &Path, title: &str, artist: &str, album: &str, track: u32, year: u32, art: &[u8]) {
    let mut file = lofty::read_from_path(path).expect("read");
    if file.primary_tag().is_none() {
        file.insert_tag(Tag::new(file.primary_tag_type()));
    }
    let t = file.primary_tag_mut().expect("tag");
    t.set_title(title.to_string());
    t.set_artist(artist.to_string());
    t.set_album(album.to_string());
    t.set_track(track);
    t.insert_text(ItemKey::AlbumArtist, artist.to_string());
    t.insert_text(ItemKey::Year, year.to_string());
    t.push_picture(
        Picture::unchecked(art.to_vec())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Jpeg)
            .build(),
    );
    file.save_to_path(path, WriteOptions::default())
        .expect("save");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "target/test-library".into());
    let albums: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(40);
    let out = Path::new(&out);
    let audio = flac(44_100 * 2, 5);
    let mut n = 0;
    for a in 0..albums {
        let artist = ARTISTS[a % ARTISTS.len()];
        let album = format!("{} {}", TITLES[(a * 5) % TITLES.len()], a + 1);
        let dir = out.join(artist).join(&album);
        std::fs::create_dir_all(&dir).expect("dir");
        let art = cover((a * 37) as u8);
        for t in 0..12u32 {
            let title = TITLES[(a + t as usize) % TITLES.len()];
            let p = dir.join(format!(
                "{:02} - {}.flac",
                t + 1,
                title.replace(['(', ')', '\''], "")
            ));
            std::fs::write(&p, &audio).expect("write");
            tag(
                &p,
                title,
                artist,
                &album,
                t + 1,
                2000 + (a as u32 % 25),
                &art,
            );
            if t % 3 == 0 {
                std::fs::write(
                    p.with_extension("lrc"),
                    format!("[00:01.00]{title}\n[00:02.00]…\n"),
                )
                .expect("lrc");
            }
            n += 1;
        }
    }
    println!("wrote {n} tracks in {albums} albums to {}", out.display());
}
