//! "Hey Bob": a few spoken commands, heard by Windows' own speech recognition, on this
//! computer (a fixed list of phrases, so it works offline and hears nothing else).

use tauri::{AppHandle, Emitter};

/// What Bob listens for. The bar turns the heard phrase into the action.
pub const PHRASES: &[&str] = &[
    "hey bob",
    "hey bob play",
    "hey bob pause",
    "hey bob next song",
    "hey bob previous song",
    "hey bob show the lyrics",
    "hey bob start focus",
    "hey bob open my notes",
    "hey bob show my computers",
    "hey bob play catch",
    "hey bob show my agents",
    "hey bob what's the weather",
    "hey bob jump",
    "hey bob go to the other computer",
    "hey bob play its sound here",
    "hey bob stop the sound",
    "hey bob close",
    "hey bob how are you",
];

#[tauri::command]
pub fn voice_set(app: AppHandle, on: bool) -> Result<(), String> {
    #[cfg(windows)]
    return imp::set(app, on);
    #[cfg(not(windows))]
    {
        let _ = (app, on);
        Err("Hey Bob works on Windows for now".into())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::Mutex;
    use windows::core::HSTRING;
    use windows_collections::IIterable;
    use windows::Foundation::TypedEventHandler;
    use windows::Media::SpeechRecognition::{
        SpeechContinuousRecognitionResultGeneratedEventArgs, SpeechContinuousRecognitionSession, SpeechRecognitionConfidence,
        SpeechRecognitionListConstraint, SpeechRecognitionResultStatus, SpeechRecognizer,
    };

    static ON: Mutex<Option<SpeechRecognizer>> = Mutex::new(None);

    pub fn set(app: AppHandle, on: bool) -> Result<(), String> {
        let mut cur = ON.lock().unwrap();
        if let Some(r) = cur.take() {
            if let Ok(s) = r.ContinuousRecognitionSession() {
                let _ = s.StopAsync().and_then(|a| a.get());
            }
            let _ = r.Close();
        }
        if !on {
            return Ok(());
        }
        let r = start(app).map_err(|e| {
            log::warn!("voice: {e}");
            "Bob can't listen: check that a microphone is on and that Windows lets desktop apps use it (Settings → Privacy → Microphone).".to_string()
        })?;
        *cur = Some(r);
        Ok(())
    }

    fn start(app: AppHandle) -> windows::core::Result<SpeechRecognizer> {
        let rec = SpeechRecognizer::new()?;
        let list: IIterable<HSTRING> = PHRASES.iter().map(|p| HSTRING::from(*p)).collect::<Vec<_>>().into();
        let c = SpeechRecognitionListConstraint::CreateWithTag(&list, &HSTRING::from("bob"))?;
        rec.Constraints()?.Append(&c)?;
        let compiled = rec.CompileConstraintsAsync()?.get()?;
        if compiled.Status()? != SpeechRecognitionResultStatus::Success {
            return Err(windows::core::Error::new(windows::core::HRESULT(-1), "speech setup failed"));
        }
        let session: SpeechContinuousRecognitionSession = rec.ContinuousRecognitionSession()?;
        session.ResultGenerated(&TypedEventHandler::<SpeechContinuousRecognitionSession, SpeechContinuousRecognitionResultGeneratedEventArgs>::new(
            move |_, args| {
                if let Some(a) = args.as_ref() {
                    let res = a.Result()?;
                    let sure = res.Confidence()?;
                    if sure == SpeechRecognitionConfidence::High || sure == SpeechRecognitionConfidence::Medium {
                        let text = res.Text()?.to_string();
                        log::info!("voice: heard \"{text}\"");
                        let _ = app.emit("voice", text);
                    }
                }
                Ok(())
            },
        ))?;
        session.StartAsync()?.get()?;
        log::info!("voice: listening for \"Hey Bob\"");
        Ok(rec)
    }
}
