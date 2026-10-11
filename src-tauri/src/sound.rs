//! Plays the island's synthesised sounds through Windows itself (`PlaySoundW`), so
//! they work no matter what the WebView does with audio.

use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

const RATE: u32 = 44_100;

/// `PlaySoundW` reads the buffer while it plays, so the last one is kept alive here.
static CURRENT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// A complete in-memory WAV file for 16-bit mono samples, scaled by `volume`.
fn wav(pcm: &[i16], volume: f32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for &s in pcm {
        let v = (f32::from(s) * volume).round().clamp(-32768.0, 32767.0) as i16;
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

pub fn play(pcm: &[i16], volume: f32) {
    if pcm.is_empty() || volume <= 0.0 {
        return;
    }
    let mut cur = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    // Stop the old sound before its buffer is replaced.
    unsafe {
        let _ = PlaySoundW(PCWSTR::null(), None, SND_ASYNC);
    }
    *cur = wav(pcm, volume);
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(cur.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}
