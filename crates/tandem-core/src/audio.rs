//! Shared sound: one computer plays its sound through the other's speakers.
//!
//! The sharing side records what it is playing (WASAPI loopback, so any app) and mutes its
//! own speakers meanwhile; the listening side plays it. Plain 16-bit stereo at 48 kHz in
//! 20 ms pieces: about 1.5 Mbit/s, nothing for a home network, and no codec to wait on.

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
/// One piece: 20 ms of 16-bit stereo.
pub const CHUNK_BYTES: usize = (RATE as usize / 50) * CHANNELS as usize * 2;

#[cfg(windows)]
pub use imp::{Capture, Player};

#[cfg(not(windows))]
pub use stub::{Capture, Player};

#[cfg(not(windows))]
mod stub {
    pub struct Capture;
    impl Capture {
        pub fn start(_on_chunk: impl FnMut(Vec<u8>) + Send + 'static) -> Result<Capture, String> {
            Err("Sharing sound works on Windows for now".into())
        }
    }
    pub struct Player;
    impl Player {
        pub fn start() -> Result<Player, String> {
            Err("Sharing sound works on Windows for now".into())
        }
        pub fn push(&self, _pcm: &[u8]) {}
    }
}

#[cfg(windows)]
mod imp {
    use super::{CHANNELS, CHUNK_BYTES, RATE};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        WAVEFORMATEX, WAVE_FORMAT_PCM,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

    fn format() -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * CHANNELS as u32 * 2,
            nBlockAlign: CHANNELS * 2,
            wBitsPerSample: 16,
            cbSize: 0,
        }
    }

    fn speakers() -> windows::core::Result<IMMDevice> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            en.GetDefaultAudioEndpoint(eRender, eConsole)
        }
    }

    /// Records everything this computer plays, in 20 ms pieces, until dropped. The speakers
    /// here are muted meanwhile (the recording is taken before the volume, so it still
    /// has the sound) and unmuted after.
    pub struct Capture {
        stop: Arc<AtomicBool>,
    }

    impl Capture {
        pub fn start(mut on_chunk: impl FnMut(Vec<u8>) + Send + 'static) -> Result<Capture, String> {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let (ok_tx, ok_rx) = std::sync::mpsc::channel::<Result<(), String>>();
            std::thread::Builder::new()
                .name("audio-capture".into())
                .spawn(move || {
                    let mut run = || -> windows::core::Result<()> {
                        let dev = speakers()?;
                        let client: IAudioClient = unsafe { dev.Activate(CLSCTX_ALL, None)? };
                        let wf = format();
                        unsafe {
                            client.Initialize(
                                AUDCLNT_SHAREMODE_SHARED,
                                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                                2_000_000,
                                0,
                                &wf,
                                None,
                            )?;
                        }
                        let cap: IAudioCaptureClient = unsafe { client.GetService()? };
                        let vol: Option<IAudioEndpointVolume> = unsafe { dev.Activate(CLSCTX_ALL, None).ok() };
                        let was_muted = vol.as_ref().and_then(|v| unsafe { v.GetMute().ok() }).map(|b| b.as_bool()).unwrap_or(false);
                        unsafe { client.Start()? };
                        if let Some(v) = &vol {
                            let _ = unsafe { v.SetMute(true, std::ptr::null()) };
                        }
                        let _ = ok_tx.send(Ok(()));
                        let mut buf: Vec<u8> = Vec::with_capacity(CHUNK_BYTES * 4);
                        while !flag.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(10));
                            loop {
                                let n = unsafe { cap.GetNextPacketSize()? };
                                if n == 0 {
                                    break;
                                }
                                let mut data = std::ptr::null_mut();
                                let mut frames = 0u32;
                                let mut flags = 0u32;
                                unsafe { cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
                                let bytes = frames as usize * CHANNELS as usize * 2;
                                if flags & 2 != 0 || data.is_null() {
                                    // Silent: no need to send it.
                                } else {
                                    buf.extend_from_slice(unsafe { std::slice::from_raw_parts(data, bytes) });
                                }
                                unsafe { cap.ReleaseBuffer(frames)? };
                            }
                            while buf.len() >= CHUNK_BYTES {
                                let rest = buf.split_off(CHUNK_BYTES);
                                on_chunk(std::mem::replace(&mut buf, rest));
                            }
                        }
                        unsafe {
                            let _ = client.Stop();
                        }
                        if let Some(v) = &vol {
                            let _ = unsafe { v.SetMute(was_muted, std::ptr::null()) };
                        }
                        Ok(())
                    };
                    if let Err(e) = run() {
                        log::warn!("audio: recording stopped: {e}");
                        let _ = ok_tx.send(Err(e.to_string()));
                    }
                })
                .map_err(|e| e.to_string())?;
            match ok_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(Ok(())) => Ok(Capture { stop }),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("The sound device didn't answer".into()),
            }
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Plays the other computer's sound here, a little behind it (60–120 ms) so the
    /// network's ups and downs don't crackle.
    pub struct Player {
        q: Arc<Mutex<VecDeque<i16>>>,
        stop: Arc<AtomicBool>,
    }

    const PREBUFFER: usize = (RATE as usize * CHANNELS as usize) * 60 / 1000;
    const MAX_BUFFER: usize = (RATE as usize * CHANNELS as usize) * 160 / 1000;

    impl Player {
        pub fn start() -> Result<Player, String> {
            let q: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (q2, flag) = (q.clone(), stop.clone());
            let (ok_tx, ok_rx) = std::sync::mpsc::channel::<Result<(), String>>();
            std::thread::Builder::new()
                .name("audio-play".into())
                .spawn(move || {
                    let run = || -> windows::core::Result<()> {
                        let dev = speakers()?;
                        let client: IAudioClient = unsafe { dev.Activate(CLSCTX_ALL, None)? };
                        let wf = format();
                        unsafe {
                            client.Initialize(
                                AUDCLNT_SHAREMODE_SHARED,
                                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                                1_000_000,
                                0,
                                &wf,
                                None,
                            )?;
                        }
                        let total = unsafe { client.GetBufferSize()? };
                        let rc: IAudioRenderClient = unsafe { client.GetService()? };
                        unsafe { client.Start()? };
                        let _ = ok_tx.send(Ok(()));
                        let mut playing = false;
                        while !flag.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(5));
                            let pad = unsafe { client.GetCurrentPadding()? };
                            let free = total.saturating_sub(pad);
                            if free == 0 {
                                continue;
                            }
                            let mut q = q2.lock().unwrap();
                            // Wait for a little in hand before starting (and after running dry).
                            if !playing && q.len() >= PREBUFFER {
                                playing = true;
                            }
                            let out = unsafe { rc.GetBuffer(free)? } as *mut i16;
                            let n = free as usize * CHANNELS as usize;
                            let dst = unsafe { std::slice::from_raw_parts_mut(out, n) };
                            for s in dst.iter_mut() {
                                *s = if playing { q.pop_front().unwrap_or(0) } else { 0 };
                            }
                            if q.is_empty() {
                                playing = false;
                            }
                            drop(q);
                            unsafe { rc.ReleaseBuffer(free, 0)? };
                        }
                        unsafe {
                            let _ = client.Stop();
                        }
                        Ok(())
                    };
                    if let Err(e) = run() {
                        log::warn!("audio: playing stopped: {e}");
                        let _ = ok_tx.send(Err(e.to_string()));
                    }
                })
                .map_err(|e| e.to_string())?;
            match ok_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(Ok(())) => Ok(Player { q, stop }),
                Ok(Err(e)) => Err(e),
                Err(_) => Err("The sound device didn't answer".into()),
            }
        }

        /// A piece from the other computer.
        pub fn push(&self, pcm: &[u8]) {
            let mut q = self.q.lock().unwrap();
            q.extend(pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)));
            // Fallen behind (a hiccup on the network): skip ahead rather than lag.
            if q.len() > MAX_BUFFER {
                let drop = q.len() - PREBUFFER;
                q.drain(..drop);
            }
        }
    }

    impl Drop for Player {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
}
