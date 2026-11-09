use super::*;
use crate::media_source::{AudioFileSource, tests::MemoryFile};
use crate::mp3_ex_oracle::Mp3Ex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

fn source(bytes: &[u8]) -> Box<dyn MediaSource> {
    Box::new(AudioFileSource::new(
        MemoryFile::new(bytes.to_vec()),
        Arc::new(AtomicBool::new(false)),
    ))
}

fn open(bytes: &[u8]) -> Minimp3 {
    Minimp3::open(source(bytes)).unwrap()
}

fn drain(source: &mut dyn PcmSource) -> Vec<f32> {
    let mut out = Vec::new();
    while source.read(&mut out).unwrap() {}
    out
}

fn with_id3v2(body: &[u8], payload: &[u8]) -> Vec<u8> {
    let n = body.len();
    let mut out = b"ID3\x03\0\0".to_vec();
    out.extend([
        (n >> 21) as u8 & 127,
        (n >> 14) as u8 & 127,
        (n >> 7) as u8 & 127,
        n as u8 & 127,
    ]);
    out.extend_from_slice(body);
    out.extend_from_slice(payload);
    out
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let mut cases: Vec<(String, Vec<u8>)> = [
        (
            "music",
            include_bytes!("decoder/mp3/fixtures/music.mp3").as_slice(),
        ),
        (
            "stereo-cbr",
            include_bytes!("decoder/mp3/fixtures/stereo-cbr.mp3"),
        ),
        (
            "mono-vbr",
            include_bytes!("decoder/mp3/fixtures/mono-vbr.mp3"),
        ),
        (
            "tiny-47-mono",
            include_bytes!("decoder/mp3/fixtures/tiny-47-mono.mp3"),
        ),
        (
            "tiny-47-stereo",
            include_bytes!("decoder/mp3/fixtures/tiny-47-stereo.mp3"),
        ),
        (
            "gapless-mpeg2",
            include_bytes!("decoder/mp3/fixtures/gapless-mpeg2.mp3"),
        ),
    ]
    .into_iter()
    .map(|(n, b)| (n.to_owned(), b.to_vec()))
    .collect();
    let music = include_bytes!("decoder/mp3/fixtures/music.mp3");
    // Untagged streams, a leading ID3v2 tag, junk before the first frame and
    // a trailing ID3v1 tag: the framing mp3dec_ex handles itself.
    cases.push(("untagged".into(), crate::mp3_fixture::stream(64)));
    cases.push(("id3v2".into(), with_id3v2(&[7; 300], music)));
    let mut junk = vec![0x55; 997];
    junk.extend_from_slice(music);
    cases.push(("leading-junk".into(), junk));
    let mut id3v1 = music.to_vec();
    id3v1.extend(b"TAG");
    id3v1.resize(id3v1.len() + 125, b'x');
    cases.push(("id3v1".into(), id3v1));
    cases
}

#[test]
fn every_stream_decodes_exactly_what_minimp3s_own_pull_reader_decodes() {
    for (name, bytes) in fixtures() {
        let mut ours = open(&bytes);
        let mut oracle = Mp3Ex::open(source(&bytes)).unwrap();
        assert_eq!(
            (ours.rate(), ours.channels(), ours.frames()),
            (oracle.rate(), oracle.channels(), oracle.frames()),
            "{name}: format and presentation length"
        );
        let (pcm, expected) = (drain(&mut ours), drain(&mut oracle));
        assert_eq!(pcm.len(), expected.len(), "{name}: length");
        assert!(pcm == expected, "{name}: PCM differs from mp3dec_ex");
    }
}

#[test]
fn every_mp3_decodes_the_product_presentation_bit_for_bit() {
    for (name, bytes) in fixtures() {
        let mut source = open(&bytes);
        let pcm = drain(&mut source);
        if let Some(frames) = source.frames() {
            assert_eq!(
                frames * u64::from(source.channels()),
                pcm.len() as u64,
                "{name}: tagged length"
            );
        }
    }
}

#[test]
fn a_seek_lands_where_mp3dec_ex_lands_and_on_the_exact_sample_of_a_whole_decode() {
    for (name, bytes) in fixtures() {
        let reference = drain(&mut open(&bytes));
        let channels = open(&bytes).channels() as usize;
        let frames = reference.len() / channels;
        for target in [
            0,
            1,
            1151,
            1152,
            frames / 3,
            frames / 2 + 5,
            frames.saturating_sub(3),
        ] {
            if target >= frames {
                continue;
            }
            let want = (frames - target).min(3000) * channels;
            let read = |source: &mut dyn PcmSource| {
                source.read(&mut Vec::new()).unwrap();
                source
                    .seek(target as u64)
                    .unwrap_or_else(|e| panic!("{name}@{target}: {e:?}"));
                let mut out = Vec::new();
                while out.len() < want && source.read(&mut out).unwrap() {}
                out.truncate(want);
                out
            };
            let ours = read(&mut open(&bytes));
            let oracle = read(&mut Mp3Ex::open(source(&bytes)).unwrap());
            assert!(
                ours == oracle,
                "{name}@{target}: seek differs from mp3dec_ex"
            );
            assert!(
                ours[..] == reference[target * channels..target * channels + want],
                "{name}@{target}: PCM after seek differs from the whole decode"
            );
        }
    }
}

/// A source that refuses to read past `limit` bytes (a body still downloading).
struct Limited {
    bytes: Vec<u8>,
    pos: u64,
    limit: Arc<AtomicU64>,
    furthest: Arc<AtomicU64>,
}
impl Read for Limited {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let limit = self
            .limit
            .load(Ordering::Acquire)
            .min(self.bytes.len() as u64);
        if self.pos >= limit && self.pos < self.bytes.len() as u64 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "not downloaded yet",
            ));
        }
        let end = (self.pos + buf.len() as u64).min(limit) as usize;
        let n = end.saturating_sub(self.pos as usize);
        buf[..n].copy_from_slice(&self.bytes[self.pos as usize..end]);
        self.pos += n as u64;
        self.furthest.fetch_max(self.pos, Ordering::AcqRel);
        Ok(n)
    }
}
impl Seek for Limited {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(n) => self.pos.checked_add_signed(n).unwrap(),
            SeekFrom::End(n) => (self.bytes.len() as u64).checked_add_signed(n).unwrap(),
        };
        Ok(self.pos)
    }
}
impl MediaSource for Limited {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.bytes.len() as u64)
    }
}

fn limited(bytes: &[u8], limit: u64) -> (Box<dyn MediaSource>, Arc<AtomicU64>, Arc<AtomicU64>) {
    let (limit, furthest) = (Arc::new(AtomicU64::new(limit)), Arc::new(AtomicU64::new(0)));
    let source = Limited {
        bytes: bytes.to_vec(),
        pos: 0,
        limit: limit.clone(),
        furthest: furthest.clone(),
    };
    (Box::new(source), limit, furthest)
}

#[test]
fn playback_starts_after_one_input_window_not_after_a_whole_io_buffer() {
    let music = include_bytes!("decoder/mp3/fixtures/music.mp3");
    let (input, _, furthest) = limited(music, 40 * 1024);
    let mut mp3 = Minimp3::open(input).unwrap();
    let mut out = Vec::new();
    assert!(mp3.read(&mut out).unwrap(), "no PCM from the first window");
    assert!(furthest.load(Ordering::Acquire) <= 40 * 1024);
    assert!(
        Mp3Ex::open(limited(music, 40 * 1024).0).is_err(),
        "mp3dec_ex waits for its whole buffer (the property this source exists for)"
    );
}

#[test]
fn a_seek_reads_only_up_to_its_target() {
    let music = include_bytes!("decoder/mp3/fixtures/music.mp3");
    let reference = drain(&mut open(music));
    let channels = 2;
    let frames = reference.len() / channels;
    let target = frames / 3;
    // A third of the presentation is about a third of the bytes; allow the
    // reservoir and one input window beyond.
    let allowed = music.len() as u64 / 3 + 48 * 1024;
    let (input, limit, furthest) = limited(music, allowed);
    let mut mp3 = Minimp3::open(input).unwrap();
    mp3.seek(target as u64).unwrap();
    let mut out = Vec::new();
    assert!(mp3.read(&mut out).unwrap());
    assert!(furthest.load(Ordering::Acquire) <= allowed);
    assert!(out[..] == reference[target * channels..target * channels + out.len()]);
    // The rest arrives and plays on to the exact end.
    limit.store(u64::MAX, Ordering::Release);
    while mp3.read(&mut out).unwrap() {}
    assert!(out[..] == reference[target * channels..]);
}

#[test]
fn a_cancelled_source_reports_cancellation_not_end_of_stream() {
    let cancel = Arc::new(AtomicBool::new(false));
    let bytes = include_bytes!("decoder/mp3/fixtures/music.mp3");
    let file = AudioFileSource::new(MemoryFile::new(bytes.to_vec()), cancel.clone());
    let mut mp3 = Minimp3::open(Box::new(file)).unwrap();
    cancel.store(true, std::sync::atomic::Ordering::Release);
    let mut out = Vec::new();
    let error = loop {
        match mp3.read(&mut out) {
            Ok(true) => continue,
            Ok(false) => panic!("cancellation read as end of stream"),
            Err(error) => break error,
        }
    };
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert!(Minimp3::open(source(&[0x55; 4096])).is_err());
}
