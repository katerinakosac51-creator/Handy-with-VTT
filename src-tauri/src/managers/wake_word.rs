use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::get_settings;
use crate::tray::{change_tray_icon, TrayIconState};
use crate::TranscriptionCoordinator;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// How long each wake-detection window captures audio before checking for the wake phrase.
const WAKE_LISTEN_SECS: u64 = 2;
/// How long each transcription segment runs before we stop it and check for the stop phrase.
const SEGMENT_SECS: u64 = 4;
/// Maximum time to wait for a transcription to appear in history after stopping a segment.
const TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, PartialEq)]
enum ListenState {
    WaitingForWake,
    Recording,
}

#[derive(Clone)]
pub struct WakeWordManager {
    app_handle: AppHandle,
    running: Arc<AtomicBool>,
}

impl WakeWordManager {
    pub fn new(app_handle: &AppHandle) -> Self {
        Self {
            app_handle: app_handle.clone(),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = self.app_handle.clone();
        let running = self.running.clone();
        std::thread::spawn(move || listen_loop(app, running));
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn sync_with_settings(&self) {
        let settings = get_settings(&self.app_handle);
        let should_run = settings.wake_word_enabled && settings.always_on_microphone;
        if should_run && !self.is_running() {
            self.start();
        } else if !should_run && self.is_running() {
            self.stop();
        }
    }
}

/// Lowercase and strip punctuation/special characters so that transcription
/// output like "Hey, Handy!" still matches the configured phrase "hey handy".
fn normalize_phrase(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_ascii_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn listen_loop(app: AppHandle, running: Arc<AtomicBool>) {
    // Kick off model loading immediately so it is ready when the wake phrase fires.
    // Without this, pure wake-word users never trigger the normal shortcut path
    // that calls initiate_model_load(), so transcribe_snippet always returns None.
    if let Some(tm) = app.try_state::<Arc<TranscriptionManager>>() {
        tm.initiate_model_load();
    }

    change_tray_icon(&app, TrayIconState::Listening);
    let mut state = ListenState::WaitingForWake;
    // Bookmark the latest history entry so we can detect new transcriptions.
    let mut last_history_id: Option<i64> = get_latest_history_id(&app);

    while running.load(Ordering::SeqCst) {
        let settings = get_settings(&app);

        if !settings.wake_word_enabled || !settings.always_on_microphone {
            change_tray_icon(&app, TrayIconState::Idle);
            running.store(false, Ordering::SeqCst);
            break;
        }

        match state {
            ListenState::WaitingForWake => {
                // Re-assert the Listening icon every iteration: other code paths
                // (e.g. a failed shortcut press while we were capturing) can reset
                // the tray to Idle without our loop knowing.
                change_tray_icon(&app, TrayIconState::Listening);

                if let Some(phrase) = capture_from_shared_stream(&app) {
                    let text_norm = normalize_phrase(&phrase);
                    let start_phrase = normalize_phrase(&settings.wake_word_phrase);

                    if text_norm.contains(&start_phrase) {
                        state = ListenState::Recording;
                        change_tray_icon(&app, TrayIconState::Recording);
                        // Snapshot history before starting so we detect only new entries.
                        last_history_id = get_latest_history_id(&app);
                        start_transcription(&app);
                        std::thread::sleep(Duration::from_millis(300));
                    }
                }
            }

            ListenState::Recording => {
                // Record a segment in 100ms ticks so we respond to stop() promptly.
                let segment_start = Instant::now();
                while running.load(Ordering::SeqCst)
                    && segment_start.elapsed() < Duration::from_secs(SEGMENT_SECS)
                {
                    std::thread::sleep(Duration::from_millis(100));
                }

                if !running.load(Ordering::SeqCst) {
                    // Loop is being stopped externally; end the active recording cleanly.
                    stop_transcription(&app);
                    break;
                }

                // Stop this segment — transcription + cursor output happens asynchronously.
                stop_transcription(&app);

                let stop_phrase = normalize_phrase(&settings.wake_word_stop_phrase);
                let new_text =
                    wait_for_new_transcription(&app, last_history_id, TRANSCRIPTION_TIMEOUT);

                // Advance bookmark regardless of whether the stop phrase was found.
                last_history_id = get_latest_history_id(&app);

                let found_stop = new_text
                    .map(|t| normalize_phrase(&t).contains(&stop_phrase))
                    .unwrap_or(false);

                if found_stop {
                    state = ListenState::WaitingForWake;
                    change_tray_icon(&app, TrayIconState::Listening);
                } else if running.load(Ordering::SeqCst) {
                    // Continue dictation: start the next segment.
                    // The coordinator may still be in Processing state because
                    // wait_for_new_transcription returns after history save but
                    // before the paste operation and ProcessingFinished arrive.
                    // Retry until recording actually starts.
                    start_transcription_with_retry(&app, &running);
                }
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    change_tray_icon(&app, TrayIconState::Idle);
}

/// Capture audio using the AudioRecordingManager's shared stream.
fn capture_from_shared_stream(app: &AppHandle) -> Option<String> {
    let audio_manager = app.try_state::<Arc<AudioRecordingManager>>()?;

    if audio_manager.is_recording() {
        std::thread::sleep(Duration::from_millis(200));
        return None;
    }

    // try_start_recording ensures the stream is open (idempotent in AlwaysOn mode).
    audio_manager.try_start_recording("wake-word").ok()?;
    std::thread::sleep(Duration::from_secs(WAKE_LISTEN_SECS));
    let audio = audio_manager.stop_recording("wake-word")?;

    if audio.is_empty() {
        return None;
    }

    // Reject audio that doesn't contain real speech energy.  VAD can still
    // trigger on brief noise bursts (HVAC, keyboard, door); those samples have
    // very low amplitude.  A person speaking a wake phrase at any normal
    // distance produces at least one 100 ms window with RMS ≥ 0.01 (≈ −40 dBFS).
    if !has_speech_energy(&audio) {
        return None;
    }

    transcribe_snippet(app, audio)
}

/// Returns true if at least one 100 ms window in `audio` (at 16 kHz) has an
/// RMS amplitude above the speech floor threshold.
fn has_speech_energy(audio: &[f32]) -> bool {
    const WINDOW_SAMPLES: usize = 1_600; // 100 ms @ 16 kHz
    const MIN_RMS: f32 = 0.01; // ≈ −40 dBFS; below this is ambient noise

    audio.chunks(WINDOW_SAMPLES).any(|chunk| {
        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
        rms >= MIN_RMS
    })
}

/// Poll HistoryManager until a new entry (different id from `last_id`) appears,
/// or until `timeout` elapses. Returns the transcription text of the new entry.
fn wait_for_new_transcription(
    app: &AppHandle,
    last_id: Option<i64>,
    timeout: Duration,
) -> Option<String> {
    let hm = app.try_state::<Arc<HistoryManager>>()?;
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if let Ok(Some(entry)) = hm.get_latest_completed_entry() {
            if Some(entry.id) != last_id {
                return Some(entry.transcription_text);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn get_latest_history_id(app: &AppHandle) -> Option<i64> {
    let hm = app.try_state::<Arc<HistoryManager>>()?;
    match hm.get_latest_completed_entry() {
        Ok(Some(entry)) => Some(entry.id),
        _ => None,
    }
}

fn transcribe_snippet(app: &AppHandle, audio: Vec<f32>) -> Option<String> {
    let tm = app.try_state::<Arc<TranscriptionManager>>()?;
    if !tm.is_model_loaded() {
        // Model is still loading; kick off a load attempt in case it hasn't
        // started yet, then let the loop retry on the next cycle.
        tm.initiate_model_load();
        return None;
    }
    match tm.transcribe(audio) {
        Ok(text) => {
            let t = text.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        }
        Err(_) => None,
    }
}

/// Start the main transcription recording and notify the frontend.
fn start_transcription(app: &AppHandle) {
    let _ = app.emit("wake-word-detected", ());
    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
        // push_to_talk=true, is_pressed=true: starts only when Idle, ignored otherwise.
        coordinator.send_input("transcribe", "wake-word", true, true);
    }
}

/// Start transcription and retry until the coordinator accepts the request.
///
/// `wait_for_new_transcription` returns as soon as the history entry is saved,
/// but the TranscriptionCoordinator stays in `Processing` state until the paste
/// operation finishes. Calling `send_input` while the coordinator is `Processing`
/// silently drops the request. This function retries every 100 ms until the
/// audio manager confirms that recording has actually started.
fn start_transcription_with_retry(app: &AppHandle, running: &Arc<AtomicBool>) {
    let _ = app.emit("wake-word-detected", ());

    let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() else {
        return;
    };
    let Some(audio_manager) = app.try_state::<Arc<AudioRecordingManager>>() else {
        return;
    };

    for _ in 0..20 {
        if !running.load(Ordering::SeqCst) {
            return;
        }
        coordinator.send_input("transcribe", "wake-word", true, true);
        std::thread::sleep(Duration::from_millis(100));
        if audio_manager.is_recording() {
            return;
        }
    }
}

/// Stop the active transcription recording (triggers transcription + cursor output).
fn stop_transcription(app: &AppHandle) {
    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
        // push_to_talk=true, is_pressed=false: stops only when Recording, ignored otherwise.
        coordinator.send_input("transcribe", "wake-word", false, true);
    }
}
