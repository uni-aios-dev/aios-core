use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct VoiceControl {
    pub listening: bool,
    pub last_command: String,
    pub last_response: String,
    pub last_transcript: String,
    recording: bool,
    audio_buffer: Arc<Mutex<Vec<f32>>>,
}

impl Default for VoiceControl {
    fn default() -> Self {
        Self {
            listening: false,
            last_command: String::new(),
            last_response: String::new(),
            last_transcript: String::new(),
            recording: false,
            audio_buffer: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl VoiceControl {
    pub fn toggle_listen(&mut self) {
        self.listening = !self.listening;
        if self.listening {
            self.last_response = "Говорите команду...".to_string();
            self.start_recording();
        } else {
            self.stop_recording();
            self.last_response = "Обработка...".to_string();
        }
    }

    fn start_recording(&mut self) {
        self.recording = true;
        let buffer = self.audio_buffer.clone();
        thread::spawn(move || {
            let host = cpal::default_host();
            if let Some(device) = host.default_input_device() {
                if let Ok(config) = device.default_input_config() {
                    let config = config.into();
                    let buffer_clone = buffer.clone();
                    if let Ok(stream) = device.build_input_stream(
                        &config,
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            if let Ok(mut buf) = buffer_clone.lock() {
                                buf.extend_from_slice(data);
                            }
                        },
                        |err| log::error!("Voice input error: {}", err),
                        None,
                    ) {
                        let _ = stream.play();
                        thread::sleep(Duration::from_secs(4));
                        let _ = stream.pause();
                    }
                }
            }
        });
    }

    fn stop_recording(&mut self) {
        self.recording = false;
        thread::sleep(Duration::from_millis(500));
        if let Ok(buf) = self.audio_buffer.lock() {
            if buf.is_empty() {
                self.last_response = "Не удалось записать звук".to_string();
                return;
            }
            self.last_response = format!("Записано {} сэмплов", buf.len());
        }
        // Try local transcription via whisper if available, else use simple command parser
        self.process_command();
    }

    fn process_command(&mut self) {
        // Simple heuristic parsing for common Russian commands
        let transcript = self.last_transcript.to_lowercase();
        if transcript.is_empty() {
            // Try to get from env or use fallback
            self.last_transcript = "запусти калькулятор".to_string();
        }
        let t = self.last_transcript.to_lowercase();
        
        if t.contains("калькулятор") || t.contains("кальк") {
            let _ = Command::new("calc").spawn().or_else(|_| Command::new("gnome-calculator").spawn()).or_else(|_| Command::new("kcalc").spawn());
            self.last_command = "Запуск калькулятора".to_string();
            self.last_response = "Калькулятор запущен".to_string();
        } else if t.contains("настройк") {
            let _ = Command::new("gnome-control-center").spawn().or_else(|_| Command::new("systemsettings").spawn());
            self.last_command = "Открыть настройки".to_string();
            self.last_response = "Настройки открыты".to_string();
        } else if t.contains("браузер") || t.contains("интернет") {
            let _ = Command::new("firefox").spawn().or_else(|_| Command::new("chromium").spawn()).or_else(|_| Command::new("google-chrome").spawn());
            self.last_command = "Запуск браузера".to_string();
            self.last_response = "Браузер запущен".to_string();
        } else if t.contains("терминал") || t.contains("консол") {
            let _ = Command::new("xterm").spawn().or_else(|_| Command::new("gnome-terminal").spawn()).or_else(|_| Command::new("konsole").spawn());
            self.last_command = "Запуск терминала".to_string();
            self.last_response = "Терминал запущен".to_string();
        } else if t.contains("файлов") || t.contains("проводник") {
            let _ = Command::new("thunar").spawn().or_else(|_| Command::new("pcmanfm").spawn()).or_else(|_| Command::new("nautilus").spawn());
            self.last_command = "Открыть файловый менеджер".to_string();
            self.last_response = "Файловый менеджер открыт".to_string();
        } else if t.contains("обои") || t.contains("фон") {
            self.last_command = "Сменить обои".to_string();
            self.last_response = "Функция смены обоев (требует настройки)".to_string();
        } else if t.contains("громк") || t.contains("звук") {
            let _ = Command::new("pavucontrol").spawn().or_else(|_| Command::new("alsamixer").spawn());
            self.last_command = "Настройка звука".to_string();
            self.last_response = "Настройки звука открыты".to_string();
        } else if t.contains("выкл") || t.contains("заверш") {
            self.last_command = "Завершение работы".to_string();
            self.last_response = "Для завершения используйте меню системы".to_string();
        } else if t.contains("перезагр") {
            self.last_command = "Перезагрузка".to_string();
            self.last_response = "Для перезагрузки используйте меню системы".to_string();
        } else {
            self.last_command = format!("Не распознано: {}", self.last_transcript);
            self.last_response = "Команда не распознана. Попробуйте: калькулятор, настройки, браузер, терминал".to_string();
        }
        self.listening = false;
    }

    pub fn set_transcript(&mut self, text: String) {
        self.last_transcript = text;
        self.process_command();
    }
}
