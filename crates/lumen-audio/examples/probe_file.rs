//! Prints what the decoder sees in a file: `cargo run -p lumen-audio --example probe_file -- <path>`.
fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: probe_file <path>");
        return;
    };
    match lumen_audio::decode::Decoder::open(std::path::Path::new(&path)) {
        Ok(mut d) => {
            println!("{:#?}", d.info());
            let mut frames = 0usize;
            let ch = d.channels();
            loop {
                match d.next_block() {
                    Ok(Some(b)) => frames += b.len() / ch,
                    Ok(None) => break,
                    Err(e) => {
                        println!("error after {frames} frames: {e}");
                        break;
                    }
                }
            }
            println!("decoded frames: {frames}");
        }
        Err(e) => println!("open failed: {e}"),
    }
}
