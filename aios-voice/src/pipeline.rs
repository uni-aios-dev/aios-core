//! Voice state machine and phase-event publication.

use crate::capture::{
    AudioFrame, AudioSource, CaptureConfig, CaptureController, CaptureMode, Level, MicrophoneSource,
};
use crate::stt::{
    CloudSttConfig, CloudSttEngine, Language, LocalWhisperEngine, SttEngine, UnavailableSttEngine,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Capacity of the phase-event bus; lagging receivers simply see the next event.
pub const EVENT_CHANNEL_CAPACITY: usize = 128;
/// Interval between level meter updates and VAD checks.
pub const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Phases exposed to the UI over the event bus and WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VoiceEvent {
    Idle,
    Listening {
        level: Level,
    },
    Transcribing,
    Transcribed {
        text: String,
    },
    Thinking,
    /// Privileged script waiting for explicit user approval.
    AwaitingConfirmation {
        script: String,
        permissions: Vec<String>,
    },
    ScriptReady {
        script: String,
        permissions: Vec<String>,
    },
    Executing,
    Completed {
        output: String,
    },
    Failed {
        reason: String,
    },
}

impl VoiceEvent {
    /// Short machine-readable phase name used by clients.
    pub fn phase(&self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Listening { .. } => "Listening",
            Self::Transcribing => "Transcribing",
            Self::Transcribed { .. } => "Transcribed",
            Self::Thinking => "Thinking",
            Self::AwaitingConfirmation { .. } => "AwaitingConfirmation",
            Self::ScriptReady { .. } => "ScriptReady",
            Self::Executing => "Executing",
            Self::Completed { .. } => "Completed",
            Self::Failed { .. } => "Failed",
        }
    }
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        log::warn!("Recovering poisoned {label} mutex");
        poisoned.into_inner()
    })
}

/// Capture + STT part of the voice pipeline. Script generation and execution
/// are driven by the bridge, which consumes the same event bus.
#[derive(Clone)]
pub struct VoicePipeline {
    events: broadcast::Sender<VoiceEvent>,
    current: Arc<Mutex<VoiceEvent>>,
    capture: Arc<Mutex<CaptureController>>,
    stt: Arc<dyn SttEngine>,
    language: Language,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl VoicePipeline {
    /// Build a pipeline with explicit audio source and STT engine.
    pub fn new(
        source: Box<dyn AudioSource>,
        stt: Arc<dyn SttEngine>,
        capture_config: CaptureConfig,
        language: Language,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let capture = CaptureController::new(source, capture_config);
        Self {
            events,
            current: Arc::new(Mutex::new(VoiceEvent::Idle)),
            capture: Arc::new(Mutex::new(capture)),
            stt,
            language,
            task: Arc::new(Mutex::new(None)),
        }
    }

    /// Production pipeline: local Whisper when a model is installed, otherwise
    /// the configured Whisper-compatible HTTP endpoint.
    pub fn with_defaults() -> Self {
        let local = LocalWhisperEngine::from_env();
        let stt: Arc<dyn SttEngine> = if local.is_available() {
            Arc::new(local)
        } else {
            let cloud = CloudSttConfig::from_env();
            if cloud.url.is_empty() {
                Arc::new(UnavailableSttEngine)
            } else {
                Arc::new(
                    CloudSttEngine::new(cloud)
                        .expect("failed to construct the configured cloud STT client"),
                )
            }
        };
        Self::new(
            Box::new(MicrophoneSource::new()),
            stt,
            CaptureConfig::default(),
            Language::Auto,
        )
    }

    /// Subscribe to phase changes.
    pub fn subscribe(&self) -> broadcast::Receiver<VoiceEvent> {
        self.events.subscribe()
    }

    /// Publish a phase change; later phases supersede earlier ones.
    pub fn emit(&self, event: VoiceEvent) {
        *lock_or_recover(&self.current, "voice state") = event.clone();
        let _ = self.events.send(event);
    }

    /// Latest published phase.
    pub fn current(&self) -> VoiceEvent {
        lock_or_recover(&self.current, "voice state").clone()
    }

    /// Recognized language setting.
    pub fn language(&self) -> Language {
        self.language
    }

    /// Start recording. `mode` overrides the configured PTT/toggle mode.
    pub fn start(&self, mode: Option<CaptureMode>) -> Result<(), String> {
        {
            let mut capture = lock_or_recover(&self.capture, "voice capture");
            if let Some(mode) = mode {
                capture.config_mut().mode = mode;
            }
            if capture.is_active() {
                return Err("capture already running".into());
            }
            capture.start()?;
        }
        self.emit(VoiceEvent::Listening {
            level: Level::default(),
        });
        let worker = self.clone();
        let handle = tokio::spawn(async move { worker.listen_loop().await });
        *lock_or_recover(&self.task, "voice task") = Some(handle);
        Ok(())
    }

    /// Stop recording and transcribe what was captured.
    pub fn stop(&self) -> Result<(), String> {
        if let Some(handle) = lock_or_recover(&self.task, "voice task").take() {
            handle.abort();
        }
        let frame = lock_or_recover(&self.capture, "voice capture")
            .stop()
            .ok_or_else(|| "no audio captured".to_string())?;
        self.finish_frame(frame);
        Ok(())
    }

    /// Transcribe an uploaded audio buffer without touching the microphone.
    pub async fn transcribe_frame(&self, frame: AudioFrame) -> Result<String, String> {
        self.emit(VoiceEvent::Transcribing);
        match self.stt.transcribe(&frame, self.language).await {
            Ok(text) => {
                self.emit(VoiceEvent::Transcribed { text: text.clone() });
                Ok(text)
            }
            Err(reason) => {
                self.emit(VoiceEvent::Failed {
                    reason: reason.to_string(),
                });
                Err(reason.to_string())
            }
        }
    }

    /// Inject a transcript from manual or text-only input into the same pipeline.
    pub fn submit_text(&self, text: String) {
        self.emit(VoiceEvent::Transcribed { text });
    }

    async fn listen_loop(&self) {
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            let status = lock_or_recover(&self.capture, "voice capture").tick();
            self.emit(VoiceEvent::Listening {
                level: status.level,
            });
            if !status.active {
                break;
            }
        }
        let frame = lock_or_recover(&self.capture, "voice capture").stop();
        if let Some(frame) = frame {
            self.finish_frame(frame);
        } else {
            self.emit(VoiceEvent::Failed {
                reason: "no audio captured".into(),
            });
        }
    }

    fn finish_frame(&self, frame: AudioFrame) {
        let worker = self.clone();
        let handle = tokio::spawn(async move {
            let _ = worker.transcribe_frame(frame).await;
        });
        *lock_or_recover(&self.task, "voice task") = Some(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::MockAudioSource;
    use crate::stt::MockSttEngine;

    #[tokio::test]
    async fn mock_audio_reaches_transcribed_phase() {
        let source = MockAudioSource::with_tone(200, 500, 0.6, 100, 16_000);
        let stt = Arc::new(MockSttEngine::with_text("открой браузер".to_string()));
        let pipeline = VoicePipeline::new(
            Box::new(source),
            stt.clone(),
            CaptureConfig {
                vad_silence_ms: 100,
                ..CaptureConfig::default()
            },
            Language::Russian,
        );
        let mut rx = pipeline.subscribe();
        pipeline.start(Some(CaptureMode::Toggle)).unwrap();
        let mut heard_levels = 0;
        let transcript = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match rx.recv().await {
                    Ok(VoiceEvent::Listening { level }) if level.rms > 0.0 => {
                        heard_levels += 1;
                    }
                    Ok(VoiceEvent::Transcribed { text }) => break text,
                    Ok(_) => {}
                    Err(e) => panic!("event channel closed: {e}"),
                }
            }
        })
        .await
        .expect("pipeline timed out");
        assert_eq!(transcript, "открой браузер");
        assert!(heard_levels > 0);
        assert_eq!(stt.call_count(), 1);
        assert_eq!(pipeline.current().phase(), "Transcribed");
    }

    #[tokio::test]
    async fn text_input_uses_same_event_bus() {
        let pipeline = VoicePipeline::new(
            Box::new(MockAudioSource::new(Vec::<Vec<f32>>::new())),
            Arc::new(MockSttEngine::with_text("unused")),
            CaptureConfig::default(),
            Language::English,
        );
        let mut rx = pipeline.subscribe();
        pipeline.submit_text("check network".into());
        assert!(matches!(
            rx.recv().await.unwrap(),
            VoiceEvent::Transcribed { text } if text == "check network"
        ));
    }

    #[tokio::test]
    async fn transcribe_upload_emits_phase_sequence() {
        let pipeline = VoicePipeline::new(
            Box::new(MockAudioSource::new(Vec::<Vec<f32>>::new())),
            Arc::new(MockSttEngine::with_text("hello".to_string())),
            CaptureConfig::default(),
            Language::English,
        );
        let mut rx = pipeline.subscribe();
        let frame = AudioFrame {
            samples: vec![0.0; 1_600],
            sample_rate: 16_000,
        };
        let text = pipeline.transcribe_frame(frame).await.unwrap();
        assert_eq!(text, "hello");
        assert_eq!(rx.recv().await.unwrap(), VoiceEvent::Transcribing);
        assert_eq!(
            rx.recv().await.unwrap(),
            VoiceEvent::Transcribed {
                text: "hello".into()
            }
        );
    }
}
