//! Voice control for the AIOS GUI.
//!
//! Captures microphone audio with `cpal`, writes a 16 kHz mono WAV and asks an
//! external speech-to-text engine for the transcript. The engine is either the
//! `AIOS_STT_CMD` environment variable or one of the well-known whisper.cpp
//! binaries (`whisper-cli`, `whisper`, `main`) found on `PATH`. Recognised
//! phrases are mapped to desktop actions (launch an application, change the
//! wallpaper, ...). When no engine is installed the GUI falls back to a text
//! field, so the feature degrades gracefully instead of pretending to listen.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Sample rate required by whisper.cpp and most STT engines.
const TARGET_RATE: u32 = 16_000;

/// Voice capture state plus the last command/transcript/response shown in the
/// status line.
pub struct VoiceControl {
    /// True while the microphone is capturing.
    pub listening: bool,
    /// Human-readable description of the last executed action.
    pub last_command: String,
    /// Result of the last action, shown next to the button.
    pub last_response: String,
    /// Last transcript (from STT or typed manually).
    pub last_transcript: String,
    /// Manual command buffer used when no STT engine is available.
    pub manual_input: String,
    /// Resolved STT command line, if any.
    pub stt: Option<String>,
    recording: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: Arc<Mutex<u32>>,
    channels: Arc<Mutex<u16>>,
}

impl Default for VoiceControl {
    fn default() -> Self {
        Self {
            listening: false,
            last_command: String::new(),
            last_response: String::new(),
            last_transcript: String::new(),
            manual_input: String::new(),
            stt: detect_stt(),
            recording: Arc::new(AtomicBool::new(false)),
            samples: Arc::new(Mutex::new(Vec::new())),
            sample_rate: Arc::new(Mutex::new(TARGET_RATE)),
            channels: Arc::new(Mutex::new(1)),
        }
    }
}

impl VoiceControl {
    /// Start or stop listening, depending on the current state.
    pub fn toggle_listen(&mut self) {
        if self.listening {
            self.stop_listen();
        } else {
            self.start_listen();
        }
    }

    fn start_listen(&mut self) {
        if self.stt.is_none() {
            self.last_response =
                "STT не найден: установите whisper.cpp (whisper-cli) или задайте AIOS_STT_CMD"
                    .to_string();
            return;
        }
        if let Ok(mut buf) = self.samples.lock() {
            buf.clear();
        }
        self.recording.store(true, Ordering::SeqCst);
        self.listening = true;
        self.last_response = "Говорите...".to_string();
        let recording = self.recording.clone();
        let samples = self.samples.clone();
        let rate = self.sample_rate.clone();
        let channels = self.channels.clone();
        thread::spawn(move || capture(recording, samples, rate, channels));
    }

    fn stop_listen(&mut self) {
        self.recording.store(false, Ordering::SeqCst);
        self.listening = false;
        thread::sleep(Duration::from_millis(250));
        let data = match self.samples.lock() {
            Ok(mut buf) => std::mem::take(&mut *buf),
            Err(_) => Vec::new(),
        };
        if data.is_empty() {
            self.last_response = "Не удалось записать звук (нет микрофона?)".to_string();
            return;
        }
        let rate = self.sample_rate.lock().map(|r| *r).unwrap_or(TARGET_RATE);
        let channels = self.channels.lock().map(|c| *c).unwrap_or(1);
        let mono = to_mono_16k(&data, rate, channels);
        let wav = data_dir().join("voice.wav");
        if let Err(e) = write_wav(&wav, &mono) {
            self.last_response = format!("Не удалось записать WAV: {e}");
            return;
        }
        match self.transcribe(&wav) {
            Ok(text) if !text.trim().is_empty() => {
                self.last_transcript = text;
                self.apply_transcript();
            }
            Ok(_) => self.last_response = "Речь не распознана".to_string(),
            Err(e) => self.last_response = format!("Ошибка STT: {e}"),
        }
    }

    /// Run the configured STT engine on a WAV file and return its transcript.
    fn transcribe(&self, wav: &Path) -> Result<String, String> {
        let stt = self.stt.clone().ok_or("STT не настроен")?;
        let mut parts = stt.split_whitespace();
        let prog = parts.next().ok_or("пустая AIOS_STT_CMD")?;
        let out = Command::new(prog)
            .args(parts)
            .arg(wav)
            .output()
            .map_err(|e| format!("{prog}: {e}"))?;
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        if text.trim().is_empty() {
            if let Ok(s) = std::fs::read_to_string(wav.with_extension("txt")) {
                text = s;
            }
        }
        if !out.status.success() && text.trim().is_empty() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(clean_transcript(&text))
    }

    /// Execute a transcript typed by the user (fallback when STT is absent).
    pub fn submit_manual(&mut self) {
        let text = self.manual_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.manual_input.clear();
        self.set_transcript(text);
    }

    /// Feed a transcript directly (also used by tests and integrations).
    pub fn set_transcript(&mut self, text: String) {
        self.last_transcript = text;
        self.apply_transcript();
    }

    fn apply_transcript(&mut self) {
        let t = self.last_transcript.to_lowercase();
        let action = dispatch(&t);
        self.last_command = action.description;
        self.last_response = action.response;
    }
}

struct Action {
    description: String,
    response: String,
}

/// Map a normalized transcript to a desktop action.
fn dispatch(t: &str) -> Action {
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));

    if has(&["калькулятор", "кальк", "calculator", "calc"]) {
        let ok = launch(&["galculator", "gnome-calculator", "kcalc", "xcalc", "calc"]);
        return done("Запуск калькулятора", ok, "Калькулятор");
    }
    if has(&["настройк", "settings", "настройки системы"]) {
        let ok = launch(&[
            "gnome-control-center",
            "systemsettings",
            "xfce4-settings-manager",
        ]);
        return done("Открыть настройки", ok, "Настройки");
    }
    if has(&["браузер", "browser", "интернет", "firefox", "chrome"]) {
        let ok = launch(&["firefox", "chromium", "google-chrome", "epiphany"]);
        return done("Запуск браузера", ok, "Браузер");
    }
    if has(&["терминал", "консол", "terminal", "console"]) {
        let ok = launch(&["xterm", "gnome-terminal", "konsole", "alacritty", "foot"]);
        return done("Запуск терминала", ok, "Терминал");
    }
    if has(&["файл", "проводник", "files", "file manager"]) {
        let ok = launch(&["thunar", "pcmanfm", "nautilus", "nemo", "dolphin"]);
        return done("Открыть файловый менеджер", ok, "Файловый менеджер");
    }
    if has(&["звук", "громк", "volume", "sound", "audio"]) {
        let ok = launch(&["pavucontrol", "alsamixer"]);
        return done("Настройка звука", ok, "Настройки звука");
    }
    if has(&["обои", "фон", "wallpaper", "background"]) {
        return change_wallpaper();
    }
    if has(&["сеть", "network", "wifi", "вайфай"]) {
        let ok = launch(&["nm-connection-editor", "gnome-control-center"]);
        return done("Настройка сети", ok, "Настройки сети");
    }
    if has(&["редактор", "editor", "текст"]) {
        let ok = launch(&["gedit", "mousepad", "leafpad", "kate"]);
        return done("Запуск редактора", ok, "Текстовый редактор");
    }
    if has(&["скриншот", "screenshot"]) {
        let ok = launch(&["gnome-screenshot", "spectacle", "scrot"]);
        return done("Скриншот", ok, "Скриншот");
    }
    if has(&["перезагруз", "reboot", "restart"]) {
        return Action {
            description: "Перезагрузка".to_string(),
            response: "Для перезагрузки используйте меню системы (нужны права root)".to_string(),
        };
    }
    if has(&["выключ", "заверш", "shutdown", "poweroff"]) {
        return Action {
            description: "Завершение работы".to_string(),
            response: "Для выключения используйте меню системы (нужны права root)".to_string(),
        };
    }

    Action {
        description: format!("Не распознано: {}", t),
        response: "Не понял. Примеры: «открой калькулятор», «запусти браузер», «смени обои»"
            .to_string(),
    }
}

fn done(desc: &str, ok: bool, what: &str) -> Action {
    Action {
        description: desc.to_string(),
        response: if ok {
            format!("{what} запущен")
        } else {
            format!("{what}: приложение не найдено")
        },
    }
}

/// Try to spawn the first available program from `candidates`.
fn launch(candidates: &[&str]) -> bool {
    candidates.iter().any(|p| {
        Command::new(p)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
    })
}

fn change_wallpaper() -> Action {
    let mut images: Vec<PathBuf> = Vec::new();
    for dir in [
        "/usr/share/backgrounds",
        "/usr/share/wallpapers",
        "/usr/share/pixmaps",
    ] {
        collect_images(Path::new(dir), &mut images, 0);
    }
    if let Ok(extra) = std::env::var("AIOS_WALLPAPERS") {
        collect_images(Path::new(&extra), &mut images, 0);
    }
    if images.is_empty() {
        return Action {
            description: "Сменить обои".to_string(),
            response: "Не найдено изображений (положите их в AIOS_WALLPAPERS)".to_string(),
        };
    }
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as usize % images.len())
        .unwrap_or(0);
    let img = &images[idx];
    let ok = ["feh", "xwallpaper", "hsetroot", "nitrogen"]
        .iter()
        .any(|tool| {
            let args: &[&str] = match *tool {
                "feh" => &["--bg-scale"],
                "xwallpaper" => &["--zoom"],
                "hsetroot" => &["-fill"],
                _ => &["--set-zoom-fill"],
            };
            Command::new(tool)
                .args(args)
                .arg(img)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .is_ok()
        });
    Action {
        description: "Сменить обои".to_string(),
        response: if ok {
            format!("Обои: {}", img.display())
        } else {
            "Нужен feh/xwallpaper/hsetroot для смены обоев".to_string()
        },
    }
}

fn collect_images(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 2 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_images(&path, out, depth + 1);
        } else if matches!(
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref(),
            Some("png" | "jpg" | "jpeg" | "webp" | "bmp")
        ) {
            out.push(path);
        }
    }
}

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("AIOS_DATA_DIR").unwrap_or_else(|_| "aios_data".into()))
}

/// Locate an STT engine: `AIOS_STT_CMD` first, then known whisper.cpp binaries.
fn detect_stt() -> Option<String> {
    if let Ok(cmd) = std::env::var("AIOS_STT_CMD") {
        if !cmd.trim().is_empty() {
            return Some(cmd);
        }
    }
    ["whisper-cli", "whisper", "main", "vosk-transcriber"]
        .iter()
        .find(|c| which(c).is_some())
        .map(|c| c.to_string())
}

fn which(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(prog);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{prog}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

/// Capture microphone audio until `recording` is cleared.
fn capture(
    recording: Arc<AtomicBool>,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: Arc<Mutex<u32>>,
    channels: Arc<Mutex<u16>>,
) {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        return;
    };
    let Ok(supported) = device.default_input_config() else {
        return;
    };
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    if let Ok(mut r) = rate.lock() {
        *r = config.sample_rate.0;
    }
    if let Ok(mut c) = channels.lock() {
        *c = config.channels;
    }
    let push = {
        let samples = samples.clone();
        move |v: f32| {
            if let Ok(mut buf) = samples.lock() {
                buf.push(v);
            }
        }
    };
    let err_fn = |e| log::error!("voice input: {e}");
    let stream = match format {
        cpal::SampleFormat::F32 => {
            let p = push.clone();
            device.build_input_stream(
                &config,
                move |d: &[f32], _: &_| d.iter().for_each(|&x| p(x)),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let p = push.clone();
            device.build_input_stream(
                &config,
                move |d: &[i16], _: &_| d.iter().for_each(|&x| p(x as f32 / 32768.0)),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let p = push.clone();
            device.build_input_stream(
                &config,
                move |d: &[u16], _: &_| d.iter().for_each(|&x| p((x as f32 - 32768.0) / 32768.0)),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::I32 => {
            let p = push.clone();
            device.build_input_stream(
                &config,
                move |d: &[i32], _: &_| d.iter().for_each(|&x| p(x as f32 / 2147483648.0)),
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::F64 => {
            let p = push.clone();
            device.build_input_stream(
                &config,
                move |d: &[f64], _: &_| d.iter().for_each(|&x| p(x as f32)),
                err_fn,
                None,
            )
        }
        _ => Err(cpal::BuildStreamError::StreamConfigNotSupported),
    };
    let Ok(stream) = stream else {
        return;
    };
    if stream.play().is_err() {
        return;
    }
    while recording.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(50));
    }
    let _ = stream.pause();
}

/// Downmix to mono and resample to [`TARGET_RATE`] with linear interpolation.
fn to_mono_16k(data: &[f32], rate: u32, channels: u16) -> Vec<f32> {
    let ch = channels.max(1) as usize;
    let mono: Vec<f32> = data
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect();
    if rate == TARGET_RATE || mono.is_empty() {
        return mono;
    }
    let ratio = rate as f32 / TARGET_RATE as f32;
    let out_len = (mono.len() as f32 / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f32 * ratio;
        let idx = pos as usize;
        let frac = pos - idx as f32;
        let a = mono[idx.min(mono.len() - 1)];
        let b = mono[(idx + 1).min(mono.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

/// Write 16-bit mono PCM WAV at [`TARGET_RATE`].
fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(v).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())
}

/// Strip whisper.cpp timestamp prefixes (`[00:00:00.000 --> ...] text`).
fn clean_transcript(raw: &str) -> String {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let text = if line.starts_with('[') {
            line.split_once(']').map(|(_, t)| t.trim()).unwrap_or(line)
        } else {
            line
        };
        if !text.is_empty() && !text.starts_with('[') {
            out.push(text);
        }
    }
    out.join(" ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_prefixes_are_stripped() {
        let raw = "[00:00:00.000 --> 00:00:02.000]  открой калькулятор\n";
        assert_eq!(clean_transcript(raw), "открой калькулятор");
    }

    #[test]
    fn calculator_intent_matches() {
        let action = dispatch("открой калькулятор");
        assert_eq!(action.description, "Запуск калькулятора");
    }

    #[test]
    fn unknown_intent_is_reported() {
        let action = dispatch("сделай что-нибудь странное");
        assert!(action.description.starts_with("Не распознано"));
    }

    #[test]
    fn resample_keeps_length_ratio() {
        let data = vec![0.0f32; 48_000];
        let mono = to_mono_16k(&data, 48_000, 1);
        assert!((mono.len() as i64 - 16_000).abs() <= 1);
    }
}
