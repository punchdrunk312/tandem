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

/// A place sound can come out of: speakers, headphones, a monitor, a headset.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Output {
    pub id: String,
    pub name: String,
    pub default: bool,
}

#[cfg(windows)]
pub use devices::{app_volume, outputs, set_app_volume, set_output};

#[cfg(not(windows))]
pub fn outputs() -> Vec<Output> {
    vec![]
}
#[cfg(not(windows))]
pub fn set_output(_id: &str) -> Result<(), String> {
    Err("Works on Windows for now".into())
}
#[cfg(not(windows))]
pub fn app_volume(_app: &str) -> Option<f32> {
    None
}
#[cfg(not(windows))]
pub fn set_app_volume(_app: &str, _v: f32) -> Result<(), String> {
    Err("Works on Windows for now".into())
}

/// Windows' sound outputs and each app's own volume (the Volume Mixer).
#[cfg(windows)]
mod devices {
    use super::Output;
    use std::ffi::c_void;
    use windows::core::{Interface, GUID, HRESULT, IUnknown, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, PROPERTYKEY};
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

    const FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };

    fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        }
    }
    unsafe fn take(p: PWSTR) -> String {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }

    pub fn outputs() -> Vec<Output> {
        let run = || -> windows::core::Result<Vec<Output>> {
            let en = enumerator()?;
            let def = unsafe { en.GetDefaultAudioEndpoint(eRender, eConsole).and_then(|d| d.GetId()).map(|p| take(p)).unwrap_or_default() };
            let all = unsafe { en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
            let mut out = vec![];
            for i in 0..unsafe { all.GetCount()? } {
                let d = unsafe { all.Item(i)? };
                let id = unsafe { take(d.GetId()?) };
                let name = unsafe { d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&FRIENDLY_NAME)).map(|v| v.to_string()).unwrap_or_default() };
                out.push(Output { default: id == def, id, name });
            }
            Ok(out)
        };
        run().unwrap_or_default()
    }

    /// Makes this output the default for everything (as the Sound settings do).
    pub fn set_output(id: &str) -> Result<(), String> {
        // IPolicyConfig: not in the SDK, but what Windows' own Sound settings use.
        #[repr(C)]
        struct Vtbl {
            _skip: [usize; 13],
            set_default: unsafe extern "system" fn(*mut c_void, PCWSTR, i32) -> HRESULT,
        }
        const CLSID: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
        const IID: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let unk: IUnknown = CoCreateInstance(&CLSID, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let mut p: *mut c_void = std::ptr::null_mut();
            unk.query(&IID, &mut p).ok().map_err(|e| e.to_string())?;
            let policy = IUnknown::from_raw(p);
            let vt = *(p as *const *const Vtbl);
            let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
            for role in 0..3 {
                ((*vt).set_default)(p, PCWSTR(wide.as_ptr()), role).ok().map_err(|e| e.to_string())?;
            }
            drop(policy);
        }
        Ok(())
    }

    fn exe_of(pid: u32) -> String {
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
            let mut buf = [0u16; 520];
            let mut n = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
            let _ = CloseHandle(h);
            if !ok {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..n as usize]);
            path.rsplit(['\\', '/']).next().unwrap_or("").trim_end_matches(".exe").to_lowercase()
        }
    }

    /// The volume controls of every sound session of this app (a browser has several).
    fn sessions(app: &str) -> Vec<ISimpleAudioVolume> {
        let want = app.to_lowercase();
        let run = || -> windows::core::Result<Vec<ISimpleAudioVolume>> {
            let en = enumerator()?;
            let dev = unsafe { en.GetDefaultAudioEndpoint(eRender, eConsole)? };
            let mgr: IAudioSessionManager2 = unsafe { dev.Activate(CLSCTX_ALL, None)? };
            let list = unsafe { mgr.GetSessionEnumerator()? };
            let mut out = vec![];
            for i in 0..unsafe { list.GetCount()? } {
                let c = unsafe { list.GetSession(i)? };
                let Ok(c2) = c.cast::<IAudioSessionControl2>() else { continue };
                let pid = unsafe { c2.GetProcessId().unwrap_or(0) };
                if pid == 0 || exe_of(pid) != want {
                    continue;
                }
                if let Ok(v) = c.cast::<ISimpleAudioVolume>() {
                    out.push(v);
                }
            }
            Ok(out)
        };
        run().unwrap_or_default()
    }

    /// This app's own volume (0..1), if it is making sound.
    pub fn app_volume(app: &str) -> Option<f32> {
        sessions(app).first().and_then(|v| unsafe { v.GetMasterVolume().ok() })
    }

    pub fn set_app_volume(app: &str, v: f32) -> Result<(), String> {
        let list = sessions(app);
        if list.is_empty() {
            return Err("That app isn't making any sound right now.".into());
        }
        for s in list {
            unsafe { s.SetMasterVolume(v.clamp(0.0, 1.0), std::ptr::null()).map_err(|e| e.to_string())? };
        }
        Ok(())
    }
}
