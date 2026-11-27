use std::sync::Arc;
use std::time::Duration;

use aios_builder::{MockScriptLlm, ScriptGenerator};
use aios_security::capability::Capability;
use aios_voice::capture::{CaptureConfig, CaptureMode, MockAudioSource};
use aios_voice::pipeline::{VoiceEvent, VoicePipeline};
use aios_voice::stt::{Language, MockSttEngine};
use aios_wasm::isolation::IsolationConfig;
use aios_wasm::sandbox::{HostPolicy, SandboxConfig, WasmBlock};

#[tokio::test]
async fn mock_audio_to_generated_wasm_execution() {
    let source = MockAudioSource::with_tone(200, 100, 0.6, 100, 16_000);
    let stt = Arc::new(MockSttEngine::with_text("notify voice test done"));
    let pipeline = VoicePipeline::new(
        Box::new(source),
        stt.clone(),
        CaptureConfig {
            vad_silence_ms: 100,
            ..CaptureConfig::default()
        },
        Language::English,
    );

    let mut events = pipeline.subscribe();
    pipeline
        .start(Some(CaptureMode::Toggle))
        .expect("capture must start");

    let transcript = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(VoiceEvent::Transcribed { text }) => break text,
                Ok(_) => {}
                Err(e) => panic!("voice event channel closed: {e}"),
            }
        }
    })
    .await
    .expect("voice pipeline timed out");

    assert_eq!(transcript, "notify voice test done");
    assert_eq!(stt.call_count(), 1);

    let llm = MockScriptLlm::once("notify \"voice test done\"\nstatus");
    let generator = ScriptGenerator::new(&llm, 2);
    let generated = generator
        .generate(&transcript, "voice_integration")
        .await
        .expect("mock LLM script must compile");
    assert!(!generated.capabilities.contains(&Capability::ProcessSpawn));
    assert_eq!(&generated.wasm[0..4], b"\x00asm");

    let mut block = WasmBlock::new(
        "voice_integration".to_string(),
        "1.0.0".to_string(),
        &generated.wasm,
        SandboxConfig::default(),
        IsolationConfig::default(),
    )
    .expect("generated WASM must load");
    let mut store = block.create_store().expect("store must be created");
    block
        .instantiate_with_policy(&mut store, HostPolicy::new(generated.capabilities.clone()))
        .expect("generated WASM must instantiate");

    let count = block
        .call_func(&mut store, "step_count", &[])
        .expect("step_count must be callable")[0]
        .i32()
        .unwrap_or(0);
    assert_eq!(count, 2);
    for index in 0..count {
        block
            .call_func(&mut store, &format!("step_{index}"), &[])
            .expect("generated step must execute");
    }

    assert_eq!(store.data().denied_calls, 0);
    assert_eq!(store.data().audit_log.len(), 3);
}
