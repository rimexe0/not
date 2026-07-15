mod ai;
mod clipboard;
mod db;

use ai::ProviderStatus;
use clipboard::ClipboardContent;
use db::{
    AiSettings, Attachment, ClipboardSettings, Database, DeletedPage, GlassSettings, Neighbors,
    Note, NoteInput, PageSummary, SaveResult, SavedWindowState,
};
use serde::Serialize;
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{
    AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, RunEvent, State, Theme,
    WebviewWindow, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

#[cfg(target_os = "macos")]
use objc2_app_kit::NSView;
#[cfg(target_os = "macos")]
use tauri::window::{Effect, EffectState, EffectsBuilder};
#[cfg(target_os = "macos")]
use window_vibrancy::{
    LiquidGlassOptions, NSGlassEffectViewStyle, apply_liquid_glass, clear_liquid_glass,
};

const PANEL_WIDTH_LOGICAL: f64 = 300.0;

struct RuntimeState {
    db: Arc<Mutex<Database>>,
    ready: AtomicBool,
    allow_exit: AtomicBool,
    panel: Mutex<Option<PanelGeometry>>,
    metrics: Mutex<VecDeque<u64>>,
    visible_metrics: Mutex<VecDeque<u64>>,
    summon_started: Mutex<Option<(u64, Instant)>>,
    summon_sequence: AtomicU64,
    first_input_count: AtomicU64,
    exit_backup_completed: AtomicBool,
    draft: Mutex<Option<NoteInput>>,
    shortcut_recording: AtomicBool,
    window_placement: Mutex<(f64, f64)>,
    window_logical_size: Mutex<(f64, f64)>,
    clipboard_generation: Arc<AtomicU64>,
    pending_clipboard: Arc<Mutex<VecDeque<PendingClipboard>>>,
    ai_cancelled: Arc<AtomicBool>,
    ai_running: AtomicBool,
}

struct PendingClipboard {
    token: String,
    content: ClipboardContent,
}

#[derive(Clone)]
struct PanelGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    side: PanelSide,
    external: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum PanelSide {
    Left,
    Right,
    Overlay,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitialState {
    note: Note,
    shortcut: String,
    shortcut_label: Option<String>,
    launch_at_login: bool,
    font_size: i64,
    theme: String,
    glass_settings: GlassSettings,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteResult {
    note: Note,
    deleted_id: Option<String>,
}

#[derive(Serialize)]
struct PanelResult {
    external: bool,
    side: PanelSide,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SummonMetrics {
    count: usize,
    p50_micros: u64,
    p95_micros: u64,
    p99_micros: u64,
    visible_count: usize,
    visible_p50_micros: u64,
    visible_p95_micros: u64,
    visible_p99_micros: u64,
    first_input_count: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardChange {
    token: String,
    kind: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AiResult {
    markdown: String,
    provider: String,
    action: String,
    duration_ms: u64,
}

impl RuntimeState {
    fn new(db: Database) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            ready: AtomicBool::new(false),
            allow_exit: AtomicBool::new(false),
            panel: Mutex::new(None),
            metrics: Mutex::new(VecDeque::with_capacity(256)),
            visible_metrics: Mutex::new(VecDeque::with_capacity(256)),
            summon_started: Mutex::new(None),
            summon_sequence: AtomicU64::new(0),
            first_input_count: AtomicU64::new(0),
            exit_backup_completed: AtomicBool::new(false),
            draft: Mutex::new(None),
            shortcut_recording: AtomicBool::new(false),
            window_placement: Mutex::new((0.5, 0.5)),
            window_logical_size: Mutex::new((440.0, 340.0)),
            clipboard_generation: Arc::new(AtomicU64::new(0)),
            pending_clipboard: Arc::new(Mutex::new(VecDeque::with_capacity(8))),
            ai_cancelled: Arc::new(AtomicBool::new(false)),
            ai_running: AtomicBool::new(false),
        }
    }

    fn record_summon(&self, micros: u64) {
        if let Ok(mut metrics) = self.metrics.lock() {
            if metrics.len() == 256 {
                metrics.pop_front();
            }
            metrics.push_back(micros);
        }
    }

    fn begin_summon(&self, started: Instant) -> u64 {
        let sequence = self.summon_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut value) = self.summon_started.lock() {
            *value = Some((sequence, started));
        }
        sequence
    }

    fn elapsed_for(&self, sequence: u64) -> Option<u64> {
        self.summon_started
            .lock()
            .ok()
            .and_then(|value| *value)
            .filter(|(current, _)| *current == sequence)
            .map(|(_, started)| started.elapsed().as_micros() as u64)
    }

    fn record_visible(&self, sequence: u64) {
        let Some(micros) = self.elapsed_for(sequence) else {
            return;
        };
        if let Ok(mut metrics) = self.visible_metrics.lock() {
            if metrics.len() == 256 {
                metrics.pop_front();
            }
            metrics.push_back(micros);
        }
    }
}

async fn database_task<T, F>(state: &State<'_, RuntimeState>, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut Database) -> Result<T, String> + Send + 'static,
{
    let database = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        let mut database = database
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        operation(&mut database)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn filtered_text_reason(body: &str, settings: &ClipboardSettings) -> Option<String> {
    let trimmed = body.trim();
    if !settings.capture_text {
        return Some("text capture is disabled".to_string());
    }
    if settings.ignore_whitespace && trimmed.is_empty() {
        return Some("clipboard contains only whitespace".to_string());
    }
    let length = body.chars().count();
    if length < settings.minimum_text_length {
        return Some("clipboard text is shorter than the configured minimum".to_string());
    }
    if length > settings.maximum_text_length {
        return Some("clipboard text is longer than the configured maximum".to_string());
    }
    if settings.ignore_sensitive && looks_sensitive(body) {
        return Some("clipboard text looks sensitive".to_string());
    }
    None
}

fn looks_sensitive(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "-----begin private key-----",
        "password=",
        "passwd=",
        "api_key=",
        "api-key:",
        "secret_key=",
        "authorization: bearer ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn utf16_to_byte_index(value: &str, target: i64) -> usize {
    let target = target.max(0) as usize;
    let mut utf16 = 0;
    for (byte, character) in value.char_indices() {
        if utf16 >= target {
            return byte;
        }
        utf16 += character.len_utf16();
    }
    value.len()
}

fn insert_after_caret_line(input: &mut NoteInput, line_end: i64, content: &str) {
    let insertion_at = utf16_to_byte_index(&input.body, line_end);
    let insertion = if input.body.is_empty() {
        content.to_string()
    } else {
        format!("\n{content}")
    };
    input.body.insert_str(insertion_at, &insertion);
}

fn insert_attachment_reference(input: &mut NoteInput, attachment_id: &str) {
    let start = utf16_to_byte_index(&input.body, input.cursor_start);
    let end = utf16_to_byte_index(&input.body, input.cursor_end.max(input.cursor_start));
    let reference = format!("![clipboard image](attachment://{attachment_id})");
    input.body.replace_range(start..end, &reference);
    let cursor = input.body[..start].encode_utf16().count() + reference.encode_utf16().count();
    input.cursor_start = cursor as i64;
    input.cursor_end = cursor as i64;
}

fn ignored_application(settings: &ClipboardSettings, bundle_identifier: Option<&str>) -> bool {
    let Some(bundle_identifier) = bundle_identifier else {
        return false;
    };
    settings
        .ignored_applications
        .split([',', '\n'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .any(|value| value.eq_ignore_ascii_case(bundle_identifier))
}

fn frontmost_application(app: &AppHandle) -> Option<String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.send(clipboard::frontmost_bundle_identifier());
    })
    .ok()?;
    receiver
        .recv_timeout(Duration::from_millis(200))
        .ok()
        .flatten()
}

fn start_clipboard_monitor(
    app: AppHandle,
    generation: Arc<AtomicU64>,
    pending: Arc<Mutex<VecDeque<PendingClipboard>>>,
    settings: ClipboardSettings,
) {
    let current = generation.fetch_add(1, Ordering::AcqRel) + 1;
    let baseline = clipboard::pasteboard_change_count();
    if let Ok(mut pending) = pending.lock() {
        pending.clear();
    }
    thread::spawn(move || {
        let mut change_count = baseline;
        let mut session_hashes = HashSet::new();
        while generation.load(Ordering::Acquire) == current {
            let next_change_count = clipboard::pasteboard_change_count();
            if next_change_count != change_count {
                change_count = next_change_count;
                let source = frontmost_application(&app);
                if source.as_deref() != Some("com.rime.not")
                    && !ignored_application(&settings, source.as_deref())
                    && let Ok(content) = clipboard::read_clipboard()
                {
                    let hash = content.hash().to_string();
                    let allowed = match &content {
                        ClipboardContent::Text { body, .. } => {
                            filtered_text_reason(body, &settings).is_none()
                        }
                        ClipboardContent::Image(_) => settings.capture_images,
                    };
                    if allowed && (!settings.ignore_duplicates || session_hashes.insert(hash)) {
                        let kind = match &content {
                            ClipboardContent::Text { .. } => "text",
                            ClipboardContent::Image(_) => "image",
                        };
                        let token = uuid::Uuid::new_v4().to_string();
                        if let Ok(mut queue) = pending.lock() {
                            if queue.len() == 8 {
                                queue.pop_front();
                            }
                            queue.push_back(PendingClipboard {
                                token: token.clone(),
                                content,
                            });
                        }
                        let _ = app.emit("clipboard-change", ClipboardChange { token, kind });
                    }
                }
            }
            thread::sleep(Duration::from_millis(150));
        }
    });
}

#[tauri::command]
async fn list_attachments(
    note_id: String,
    state: State<'_, RuntimeState>,
) -> Result<Vec<Attachment>, String> {
    database_task(&state, move |db| db.list_attachments(&note_id)).await
}

#[tauri::command]
async fn paste_clipboard_image(
    mut input: NoteInput,
    state: State<'_, RuntimeState>,
) -> Result<Note, String> {
    let image = tauri::async_runtime::spawn_blocking(clipboard::read_clipboard_image)
        .await
        .map_err(|error| error.to_string())??;
    let attachment_id = uuid::Uuid::new_v4().to_string();
    insert_attachment_reference(&mut input, &attachment_id);
    database_task(&state, move |db| {
        db.save_note_with_attachment(
            &input,
            &attachment_id,
            &image.png,
            &image.thumbnail_png,
            image.width,
            image.height,
            &image.hash,
            "manual",
            None,
        )
    })
    .await
}

#[tauri::command]
async fn start_visible_clipboard(
    app: AppHandle,
    state: State<'_, RuntimeState>,
) -> Result<(), String> {
    let settings = database_task(&state, |db| db.clipboard_settings()).await?;
    start_clipboard_monitor(
        app,
        Arc::clone(&state.clipboard_generation),
        Arc::clone(&state.pending_clipboard),
        settings,
    );
    Ok(())
}

#[tauri::command]
fn stop_visible_clipboard(state: State<'_, RuntimeState>) {
    state.clipboard_generation.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut queue) = state.pending_clipboard.lock() {
        queue.clear();
    }
}

#[tauri::command]
async fn append_clipboard_change(
    token: String,
    mut input: NoteInput,
    expected_updated_at: i64,
    caret_line_end: i64,
    state: State<'_, RuntimeState>,
) -> Result<Note, String> {
    let pending = {
        let mut queue = state
            .pending_clipboard
            .lock()
            .map_err(|_| "clipboard queue lock poisoned".to_string())?;
        let index = queue
            .iter()
            .position(|item| item.token == token)
            .ok_or_else(|| "clipboard change expired".to_string())?;
        queue
            .remove(index)
            .ok_or_else(|| "clipboard change expired".to_string())?
    };
    let database = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        let mut db = database
            .lock()
            .map_err(|_| "database lock poisoned".to_string())?;
        if !db.revision_matches(&input.id, expected_updated_at, input.persisted)? {
            return Err("the note changed before clipboard insertion".to_string());
        }
        match pending.content {
            ClipboardContent::Text { body, .. } => {
                insert_after_caret_line(&mut input, caret_line_end, &body);
                db.save_note(&input)?;
                db.select_note(&input.id)
            }
            ClipboardContent::Image(image) => {
                let attachment_id = uuid::Uuid::new_v4().to_string();
                let reference = format!("![clipboard image](attachment://{attachment_id})");
                insert_after_caret_line(&mut input, caret_line_end, &reference);
                db.save_note_with_attachment(
                    &input,
                    &attachment_id,
                    &image.png,
                    &image.thumbnail_png,
                    image.width,
                    image.height,
                    &image.hash,
                    "clipboard_image",
                    Some(&image.hash),
                )
            }
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn clipboard_settings(state: State<'_, RuntimeState>) -> Result<ClipboardSettings, String> {
    database_task(&state, |db| db.clipboard_settings()).await
}

#[tauri::command]
async fn set_clipboard_settings(
    settings: ClipboardSettings,
    state: State<'_, RuntimeState>,
) -> Result<ClipboardSettings, String> {
    database_task(&state, move |db| db.set_clipboard_settings(settings)).await
}

#[tauri::command]
async fn ai_settings(state: State<'_, RuntimeState>) -> Result<AiSettings, String> {
    database_task(&state, |db| db.ai_settings()).await
}

#[tauri::command]
async fn set_ai_settings(
    settings: AiSettings,
    state: State<'_, RuntimeState>,
) -> Result<AiSettings, String> {
    database_task(&state, move |db| db.set_ai_settings(&settings)).await
}

#[tauri::command]
async fn detect_ai_providers(
    state: State<'_, RuntimeState>,
) -> Result<Vec<ProviderStatus>, String> {
    let settings = database_task(&state, |db| db.ai_settings()).await?;
    tauri::async_runtime::spawn_blocking(move || ai::detect_providers(&settings))
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn run_ai(
    provider: String,
    action: String,
    body: String,
    state: State<'_, RuntimeState>,
) -> Result<AiResult, String> {
    if state
        .ai_running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("another AI request is already running".to_string());
    }
    state.ai_cancelled.store(false, Ordering::Release);
    let started = Instant::now();
    let settings = database_task(&state, |db| db.ai_settings()).await;
    let result_provider = provider.clone();
    let result_action = action.clone();
    let result = match settings {
        Ok(settings) => {
            let cancelled = Arc::clone(&state.ai_cancelled);
            tauri::async_runtime::spawn_blocking(move || {
                ai::run_provider(&provider, &action, &body, &settings, cancelled)
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result)
        }
        Err(error) => Err(error),
    };
    state.ai_running.store(false, Ordering::Release);
    let markdown = result?;
    Ok(AiResult {
        markdown,
        provider: result_provider,
        action: result_action,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

#[tauri::command]
fn cancel_ai(state: State<'_, RuntimeState>) {
    state.ai_cancelled.store(true, Ordering::Release);
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    if !safe_external_url(&url) {
        return Err("unsupported link".to_string());
    }
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn safe_external_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://") || url.starts_with("mailto:"))
        && !url.chars().any(char::is_whitespace)
}

#[cfg(target_os = "macos")]
fn glass_tint(settings: &GlassSettings, light: bool) -> (u8, u8, u8, u8) {
    let tint = if light {
        &settings.light_tint
    } else {
        &settings.dark_tint
    };
    let component = |range| u8::from_str_radix(&tint[range], 16).unwrap_or_default();
    let alpha = ((u16::from(settings.opacity) * 255 + 50) / 100) as u8;
    (component(1..3), component(3..5), component(5..7), alpha)
}

fn apply_window_material(
    window: &WebviewWindow,
    color_theme: &str,
    system_theme: Theme,
    glass_settings: &GlassSettings,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let glass_enabled = glass_settings.enabled;
        let light =
            color_theme == "light" || (color_theme == "auto" && system_theme == Theme::Light);
        let tint = glass_tint(glass_settings, light);
        let window = window.clone();
        window
            .clone()
            .with_webview(move |webview| {
                let empty_effects = || EffectsBuilder::new().build();
                let _ = clear_liquid_glass(&window);
                if !glass_enabled {
                    let _ = window.set_effects(empty_effects());
                    return;
                }

                let _ = window.set_effects(empty_effects());
                let webview = unsafe { &*webview.inner().cast::<NSView>() };
                let options = LiquidGlassOptions::new(NSGlassEffectViewStyle::Clear)
                    .tint_color(tint)
                    .radius(14.0)
                    .content_view(webview);
                if apply_liquid_glass(&window, options).is_err() {
                    let fallback = EffectsBuilder::new()
                        .effect(Effect::UnderWindowBackground)
                        .state(EffectState::FollowsWindowActiveState)
                        .radius(14.0)
                        .build();
                    let _ = window.set_effects(fallback);
                }
            })
            .map_err(|error| error.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (window, color_theme, system_theme, glass_settings);
    Ok(())
}

#[tauri::command]
async fn load_initial_state(
    app: AppHandle,
    state: State<'_, RuntimeState>,
) -> Result<InitialState, String> {
    let (note, shortcut, shortcut_label, launch_at_login, font_size, theme, glass_settings) =
        database_task(&state, |db| {
            Ok((
                db.initial_note()?,
                db.shortcut()?,
                db.shortcut_label()?,
                db.launch_at_login()?,
                db.font_size()?,
                db.theme()?,
                db.glass_settings()?,
            ))
        })
        .await?;
    let actual_autostart = app.autolaunch().is_enabled().unwrap_or(false);
    Ok(InitialState {
        note,
        shortcut,
        shortcut_label,
        launch_at_login: launch_at_login && actual_autostart,
        font_size,
        theme,
        glass_settings,
    })
}

#[tauri::command]
fn mark_ready(state: State<'_, RuntimeState>) {
    state.ready.store(true, Ordering::Release);
}

#[tauri::command]
fn set_shortcut_recording(recording: bool, state: State<'_, RuntimeState>) {
    state.shortcut_recording.store(recording, Ordering::Release);
}

#[tauri::command]
async fn save_note(input: NoteInput, state: State<'_, RuntimeState>) -> Result<SaveResult, String> {
    database_task(&state, move |db| db.save_note(&input)).await
}

#[tauri::command]
fn update_draft(input: NoteInput, state: State<'_, RuntimeState>) -> Result<(), String> {
    *state
        .draft
        .lock()
        .map_err(|_| "draft lock poisoned".to_string())? = Some(input);
    Ok(())
}

#[tauri::command]
fn update_draft_view(
    note_id: String,
    cursor_start: i64,
    cursor_end: i64,
    scroll_top: f64,
    state: State<'_, RuntimeState>,
) -> Result<(), String> {
    let mut draft = state
        .draft
        .lock()
        .map_err(|_| "draft lock poisoned".to_string())?;
    if let Some(input) = draft.as_mut()
        && input.id == note_id
    {
        input.cursor_start = cursor_start;
        input.cursor_end = cursor_end;
        input.scroll_top = scroll_top;
    }
    Ok(())
}

#[tauri::command]
async fn navigate(
    note_id: String,
    direction: i32,
    state: State<'_, RuntimeState>,
) -> Result<Note, String> {
    database_task(&state, move |db| db.navigate(&note_id, direction)).await
}

#[tauri::command]
async fn neighbors(note_id: String, state: State<'_, RuntimeState>) -> Result<Neighbors, String> {
    database_task(&state, move |db| db.neighbors(&note_id)).await
}

#[tauri::command]
async fn select_note(note_id: String, state: State<'_, RuntimeState>) -> Result<Note, String> {
    database_task(&state, move |db| db.select_note(&note_id)).await
}

#[tauri::command]
async fn delete_note(
    note_id: String,
    state: State<'_, RuntimeState>,
) -> Result<DeleteResult, String> {
    let (note, deleted_id) = database_task(&state, move |db| db.delete_note(&note_id)).await?;
    Ok(DeleteResult { note, deleted_id })
}

#[tauri::command]
async fn restore_note(note_id: String, state: State<'_, RuntimeState>) -> Result<Note, String> {
    database_task(&state, move |db| db.restore_note(&note_id)).await
}

#[tauri::command]
async fn list_pages(
    query: String,
    state: State<'_, RuntimeState>,
) -> Result<Vec<PageSummary>, String> {
    database_task(&state, move |db| db.list_pages(&query)).await
}

#[tauri::command]
async fn list_deleted(state: State<'_, RuntimeState>) -> Result<Vec<DeletedPage>, String> {
    database_task(&state, |db| db.list_deleted()).await
}

#[tauri::command]
async fn set_shortcut(
    shortcut: String,
    label: String,
    app: AppHandle,
    state: State<'_, RuntimeState>,
) -> Result<String, String> {
    let candidate = shortcut.trim().to_string();
    if candidate.is_empty() {
        return Err("shortcut cannot be empty".to_string());
    }
    let previous = database_task(&state, |db| db.shortcut()).await?;
    app.global_shortcut()
        .unregister(previous.as_str())
        .map_err(|error| error.to_string())?;
    if let Err(error) = app.global_shortcut().register(candidate.as_str()) {
        let _ = app.global_shortcut().register(previous.as_str());
        return Err(error.to_string());
    }
    let saved = candidate.clone();
    if let Err(error) = database_task(&state, move |db| db.set_shortcut(&saved, &label)).await {
        let _ = app.global_shortcut().unregister(candidate.as_str());
        let _ = app.global_shortcut().register(previous.as_str());
        return Err(error);
    }
    Ok(candidate)
}

#[tauri::command]
async fn set_autostart(
    enabled: bool,
    app: AppHandle,
    state: State<'_, RuntimeState>,
) -> Result<bool, String> {
    if enabled {
        app.autolaunch()
            .enable()
            .map_err(|error| error.to_string())?;
    } else {
        app.autolaunch()
            .disable()
            .map_err(|error| error.to_string())?;
    }
    database_task(&state, move |db| db.set_launch_at_login(enabled)).await?;
    app.autolaunch()
        .is_enabled()
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn set_font_size(value: i64, state: State<'_, RuntimeState>) -> Result<i64, String> {
    database_task(&state, move |db| db.set_font_size(value)).await
}

#[tauri::command]
async fn set_theme(
    value: String,
    window: WebviewWindow,
    state: State<'_, RuntimeState>,
) -> Result<String, String> {
    let (theme, glass_settings) = database_task(&state, move |db| {
        Ok((db.set_theme(&value)?, db.glass_settings()?))
    })
    .await?;
    let system_theme = window.theme().unwrap_or(Theme::Dark);
    apply_window_material(&window, &theme, system_theme, &glass_settings)?;
    Ok(theme)
}

#[tauri::command]
async fn set_glass_settings(
    settings: GlassSettings,
    window: WebviewWindow,
    state: State<'_, RuntimeState>,
) -> Result<GlassSettings, String> {
    let (settings, theme) = database_task(&state, move |db| {
        Ok((db.set_glass_settings(settings)?, db.theme()?))
    })
    .await?;
    let system_theme = window.theme().unwrap_or(Theme::Dark);
    apply_window_material(&window, &theme, system_theme, &settings)?;
    Ok(settings)
}

#[tauri::command]
fn set_panel(
    open: bool,
    panel_width_logical: Option<f64>,
    window: WebviewWindow,
    state: State<'_, RuntimeState>,
) -> Result<PanelResult, String> {
    let mut panel = state
        .panel
        .lock()
        .map_err(|_| "panel lock poisoned".to_string())?;
    if !open {
        if let Some(geometry) = panel.take() {
            if geometry.external {
                window
                    .set_size(PhysicalSize::new(geometry.width, geometry.height))
                    .map_err(|error| error.to_string())?;
                window
                    .set_position(PhysicalPosition::new(geometry.x, geometry.y))
                    .map_err(|error| error.to_string())?;
            }
            window
                .set_resizable(true)
                .map_err(|error| error.to_string())?;
        }
        return Ok(PanelResult {
            external: true,
            side: PanelSide::Right,
        });
    }

    if let Some(geometry) = panel.as_ref() {
        return Ok(PanelResult {
            external: geometry.external,
            side: geometry.side,
        });
    }

    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let monitor = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .or(window
            .primary_monitor()
            .map_err(|error| error.to_string())?)
        .ok_or_else(|| "no display available".to_string())?;
    let panel_width_logical = panel_width_logical
        .unwrap_or(PANEL_WIDTH_LOGICAL)
        .clamp(260.0, 420.0);
    let panel_width = (panel_width_logical * scale).round() as u32;
    let work_area = monitor.work_area();
    let right_space = i64::from(work_area.position.x) + i64::from(work_area.size.width)
        - i64::from(position.x)
        - i64::from(size.width);
    let left_space = i64::from(position.x) - i64::from(work_area.position.x);

    let (side, external) = choose_panel_side(right_space, left_space, i64::from(panel_width));
    let geometry = PanelGeometry {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
        side,
        external,
    };

    if external {
        let (expanded_x, expanded_width) =
            expanded_panel_window(position.x, size.width, panel_width, side);
        window
            .set_resizable(false)
            .map_err(|error| error.to_string())?;
        window
            .set_size(PhysicalSize::new(expanded_width, size.height))
            .map_err(|error| error.to_string())?;
        if expanded_x != position.x {
            window
                .set_position(PhysicalPosition::new(expanded_x, position.y))
                .map_err(|error| error.to_string())?;
        }
    }
    *panel = Some(geometry);
    Ok(PanelResult { external, side })
}

#[tauri::command]
async fn save_window_state(
    window: WebviewWindow,
    state: State<'_, RuntimeState>,
) -> Result<(), String> {
    if state
        .panel
        .lock()
        .map_err(|_| "panel lock poisoned".to_string())?
        .is_some()
    {
        return Ok(());
    }
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let monitor = window
        .current_monitor()
        .map_err(|error| error.to_string())?;
    let display_identifier = monitor.as_ref().map(monitor_identifier);
    let mut placement = state
        .window_placement
        .lock()
        .map(|value| *value)
        .unwrap_or((0.5, 0.5));
    if let Some(monitor) = monitor {
        let area = monitor.work_area();
        placement = relative_placement(
            position.x,
            position.y,
            size.width,
            size.height,
            area.position.x,
            area.position.y,
            area.size.width,
            area.size.height,
        );
        if let Ok(mut current) = state.window_placement.lock() {
            *current = placement;
        }
    }
    let logical_size = (
        f64::from(size.width) / scale.max(0.1),
        f64::from(size.height) / scale.max(0.1),
    );
    if let Ok(mut current) = state.window_logical_size.lock() {
        *current = logical_size;
    }
    database_task(&state, move |db| {
        db.save_window_state(&SavedWindowState {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            scale_factor: scale,
            display_identifier,
            relative_x: Some(placement.0),
            relative_y: Some(placement.1),
            logical_width: Some(logical_size.0),
            logical_height: Some(logical_size.1),
        })
    })
    .await
}

#[tauri::command]
async fn export_notes(state: State<'_, RuntimeState>) -> Result<String, String> {
    let path = database_task(&state, |db| db.export_markdown()).await?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn hide_window(window: WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|error| error.to_string())
}

#[tauri::command]
fn start_window_drag(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|error| error.to_string())
}

#[tauri::command]
async fn quit_app(app: AppHandle, state: State<'_, RuntimeState>) -> Result<(), String> {
    let draft = state
        .draft
        .lock()
        .map_err(|_| "draft lock poisoned".to_string())?
        .clone();
    database_task(&state, move |db| {
        persist_draft_and_backup(db, draft.as_ref())
    })
    .await?;
    state.exit_backup_completed.store(true, Ordering::Release);
    state.allow_exit.store(true, Ordering::Release);
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn summon_metrics(state: State<'_, RuntimeState>) -> SummonMetrics {
    let mut values = state
        .metrics
        .lock()
        .map(|items| items.iter().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    values.sort_unstable();
    let mut visible_values = state
        .visible_metrics
        .lock()
        .map(|items| items.iter().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    visible_values.sort_unstable();
    SummonMetrics {
        count: values.len(),
        p50_micros: percentile(&values, 0.50),
        p95_micros: percentile(&values, 0.95),
        p99_micros: percentile(&values, 0.99),
        visible_count: visible_values.len(),
        visible_p50_micros: percentile(&visible_values, 0.50),
        visible_p95_micros: percentile(&visible_values, 0.95),
        visible_p99_micros: percentile(&visible_values, 0.99),
        first_input_count: state.first_input_count.load(Ordering::Relaxed),
    }
}

#[tauri::command]
fn record_visible_frame(sequence: u64, state: State<'_, RuntimeState>) {
    state.record_visible(sequence);
}

#[tauri::command]
fn record_first_input(sequence: u64, state: State<'_, RuntimeState>) {
    if state.elapsed_for(sequence).is_some() {
        state.first_input_count.fetch_add(1, Ordering::Relaxed);
    }
}

fn percentile(values: &[u64], percentile: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let index = ((values.len() - 1) as f64 * percentile).round() as usize;
    values[index]
}

fn persist_draft_and_backup(
    database: &mut Database,
    draft: Option<&NoteInput>,
) -> Result<(), String> {
    if let Some(input) = draft {
        database.save_note(input)?;
    }
    database.backup().map(|_| ())
}

fn restore_window(window: &WebviewWindow, state: &RuntimeState) {
    let Ok(database) = state.db.lock() else {
        return;
    };
    let Ok(Some(saved)) = database.window_state() else {
        return;
    };
    drop(database);
    let monitors = window.available_monitors().unwrap_or_default();
    let target = saved
        .display_identifier
        .as_ref()
        .and_then(|identifier| {
            monitors
                .iter()
                .find(|monitor| monitor_identifier(monitor) == *identifier)
        })
        .or_else(|| {
            monitors.iter().find(|monitor| {
                let area = monitor.work_area();
                saved.x >= area.position.x
                    && saved.x < area.position.x + area.size.width as i32
                    && saved.y >= area.position.y
                    && saved.y < area.position.y + area.size.height as i32
            })
        })
        .or_else(|| monitors.first());
    let current_scale = target
        .map(Monitor::scale_factor)
        .unwrap_or(saved.scale_factor);
    let logical_size = (
        saved
            .logical_width
            .unwrap_or(f64::from(saved.width) / saved.scale_factor.max(0.1)),
        saved
            .logical_height
            .unwrap_or(f64::from(saved.height) / saved.scale_factor.max(0.1)),
    );
    let placement = target
        .map(|monitor| {
            if let (Some(relative_x), Some(relative_y)) = (saved.relative_x, saved.relative_y) {
                (relative_x.clamp(0.0, 1.0), relative_y.clamp(0.0, 1.0))
            } else {
                let area = monitor.work_area();
                relative_placement(
                    saved.x,
                    saved.y,
                    saved.width,
                    saved.height,
                    area.position.x,
                    area.position.y,
                    area.size.width,
                    area.size.height,
                )
            }
        })
        .unwrap_or((0.5, 0.5));
    let (x, y, width, height) = target
        .map(|monitor| {
            let area = monitor.work_area();
            restored_geometry(
                placement,
                logical_size,
                current_scale,
                (
                    area.position.x,
                    area.position.y,
                    area.size.width,
                    area.size.height,
                ),
            )
        })
        .unwrap_or((saved.x, saved.y, saved.width, saved.height));
    if let Ok(mut current) = state.window_placement.lock() {
        *current = placement;
    }
    if let Ok(mut current) = state.window_logical_size.lock() {
        *current = logical_size;
    }
    let _ = window.set_size(PhysicalSize::new(width, height));
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

#[cfg(target_os = "macos")]
fn move_window_to_cursor_monitor(window: &WebviewWindow, state: &RuntimeState) {
    let placement = state
        .window_placement
        .lock()
        .map(|value| *value)
        .unwrap_or((0.5, 0.5));
    let logical_size = state
        .window_logical_size
        .lock()
        .map(|value| *value)
        .unwrap_or((440.0, 340.0));
    let window = window.clone();
    let _ = window.clone().run_on_main_thread(move || {
        move_window_to_cursor_monitor_on_main_thread(&window, placement, logical_size);
    });
}

#[cfg(target_os = "macos")]
fn move_window_to_cursor_monitor_on_main_thread(
    window: &WebviewWindow,
    fallback_placement: (f64, f64),
    logical_size: (f64, f64),
) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSScreen, NSWindow};

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let Ok(native_window) = window.ns_window() else {
        return;
    };
    let native_window: &NSWindow = unsafe { &*native_window.cast() };
    let cursor = NSEvent::mouseLocation();
    let screens = NSScreen::screens(main_thread);
    let current_frame = native_window.frame();
    let window_center = (
        current_frame.origin.x + current_frame.size.width / 2.0,
        current_frame.origin.y + current_frame.size.height / 2.0,
    );
    let local_offset = screens
        .iter()
        .find(|screen| {
            let frame = screen.frame();
            window_center.0 >= frame.origin.x
                && window_center.0 < frame.origin.x + frame.size.width
                && window_center.1 >= frame.origin.y
                && window_center.1 < frame.origin.y + frame.size.height
        })
        .map(|screen| {
            let area = screen.visibleFrame();
            appkit_local_offset(
                current_frame.origin.x,
                current_frame.origin.y,
                current_frame.size.width,
                current_frame.size.height,
                area.origin.x,
                area.origin.y,
                area.size.width,
                area.size.height,
            )
        });
    let Some(target) = screens.iter().find(|screen| {
        let frame = screen.frame();
        cursor.x >= frame.origin.x
            && cursor.x < frame.origin.x + frame.size.width
            && cursor.y >= frame.origin.y
            && cursor.y < frame.origin.y + frame.size.height
    }) else {
        return;
    };
    let target_area = target.visibleFrame();
    let (x, y) = local_offset.map_or_else(
        || {
            position_for_placement(
                fallback_placement,
                logical_size.0,
                logical_size.1,
                target_area.origin.x,
                target_area.origin.y,
                target_area.size.width,
                target_area.size.height,
            )
        },
        |offset| {
            position_for_local_offset(
                offset,
                logical_size.0,
                logical_size.1,
                target_area.origin.x,
                target_area.origin.y,
                target_area.size.width,
                target_area.size.height,
            )
        },
    );
    let mut frame = native_window.frame();
    frame.origin.x = x;
    frame.origin.y = y;
    frame.size.width = logical_size.0;
    frame.size.height = logical_size.1;
    native_window.setFrame_display(frame, true);
}

#[cfg(not(target_os = "macos"))]
fn move_window_to_cursor_monitor(_window: &WebviewWindow, _state: &RuntimeState) {}

#[cfg(target_os = "macos")]
fn configure_native_scratchpad_window(window: &WebviewWindow) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSFloatingWindowLevel, NSWindow, NSWindowCollectionBehavior};

    let Some(_main_thread) = MainThreadMarker::new() else {
        return;
    };
    let Ok(native_window) = window.ns_window() else {
        return;
    };
    let native_window: &NSWindow = unsafe { &*native_window.cast() };
    native_window.setLevel(NSFloatingWindowLevel);
    native_window.setHidesOnDeactivate(false);
    native_window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
}

#[cfg(not(target_os = "macos"))]
fn configure_native_scratchpad_window(_window: &WebviewWindow) {}

fn monitor_identifier(monitor: &Monitor) -> String {
    let position = monitor.position();
    let size = monitor.size();
    format!(
        "{}@{}:{}:{}x{}",
        monitor.name().map(String::as_str).unwrap_or("display"),
        position.x,
        position.y,
        size.width,
        size.height
    )
}

#[allow(clippy::too_many_arguments)]
fn relative_placement(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    bounds_x: i32,
    bounds_y: i32,
    bounds_width: u32,
    bounds_height: u32,
) -> (f64, f64) {
    fn axis(value: i32, size: u32, start: i32, bounds_size: u32) -> f64 {
        let travel = bounds_size.saturating_sub(size);
        if travel == 0 {
            return 0.0;
        }
        (f64::from(value - start) / f64::from(travel)).clamp(0.0, 1.0)
    }

    (
        axis(x, width, bounds_x, bounds_width),
        axis(y, height, bounds_y, bounds_height),
    )
}

fn position_for_placement(
    placement: (f64, f64),
    width: f64,
    height: f64,
    bounds_x: f64,
    bounds_y: f64,
    bounds_width: f64,
    bounds_height: f64,
) -> (f64, f64) {
    (
        bounds_x + placement.0.clamp(0.0, 1.0) * (bounds_width - width).max(0.0),
        bounds_y + (1.0 - placement.1.clamp(0.0, 1.0)) * (bounds_height - height).max(0.0),
    )
}

#[allow(clippy::too_many_arguments)]
fn appkit_local_offset(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    bounds_x: f64,
    bounds_y: f64,
    bounds_width: f64,
    bounds_height: f64,
) -> (f64, f64) {
    (
        (x - bounds_x).clamp(0.0, (bounds_width - width).max(0.0)),
        (bounds_y + bounds_height - y - height).clamp(0.0, (bounds_height - height).max(0.0)),
    )
}

#[allow(clippy::too_many_arguments)]
fn position_for_local_offset(
    offset: (f64, f64),
    width: f64,
    height: f64,
    bounds_x: f64,
    bounds_y: f64,
    bounds_width: f64,
    bounds_height: f64,
) -> (f64, f64) {
    let x_offset = offset.0.clamp(0.0, (bounds_width - width).max(0.0));
    let top_offset = offset.1.clamp(0.0, (bounds_height - height).max(0.0));
    (
        bounds_x + x_offset,
        bounds_y + bounds_height - height - top_offset,
    )
}

fn restored_geometry(
    placement: (f64, f64),
    logical_size: (f64, f64),
    scale: f64,
    bounds: (i32, i32, u32, u32),
) -> (i32, i32, u32, u32) {
    let width = (logical_size.0 * scale).round() as u32;
    let height = (logical_size.1 * scale).round() as u32;
    let x = bounds.0
        + (placement.0.clamp(0.0, 1.0) * f64::from(bounds.2.saturating_sub(width))).round() as i32;
    let y = bounds.1
        + (placement.1.clamp(0.0, 1.0) * f64::from(bounds.3.saturating_sub(height))).round() as i32;
    (x, y, width, height)
}

fn choose_panel_side(right_space: i64, left_space: i64, panel_width: i64) -> (PanelSide, bool) {
    if right_space >= panel_width {
        (PanelSide::Right, true)
    } else if left_space >= panel_width {
        (PanelSide::Left, true)
    } else {
        (PanelSide::Overlay, false)
    }
}

fn expanded_panel_window(
    editor_x: i32,
    editor_width: u32,
    panel_width: u32,
    side: PanelSide,
) -> (i32, u32) {
    let x = if matches!(side, PanelSide::Left) {
        editor_x - panel_width as i32
    } else {
        editor_x
    };
    (x, editor_width + panel_width)
}

pub fn run() {
    let app = tauri::Builder::default()
        .register_uri_scheme_protocol("not-asset", |context, request| {
            let id = request
                .uri()
                .path()
                .strip_prefix("/thumbnail/")
                .unwrap_or("");
            if id.is_empty()
                || !id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
            {
                return tauri::http::Response::builder()
                    .status(400)
                    .body(Vec::new())
                    .unwrap();
            }
            let state = context.app_handle().state::<RuntimeState>();
            let response = state
                .db
                .lock()
                .map_err(|_| "database lock poisoned".to_string())
                .and_then(|mut db| db.active_attachment_thumbnail(id));
            match response {
                Ok((bytes, mime_type)) => tauri::http::Response::builder()
                    .header(tauri::http::header::CONTENT_TYPE, mime_type)
                    .header(tauri::http::header::CACHE_CONTROL, "no-store")
                    .body(bytes)
                    .unwrap(),
                Err(_) => tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    let state = app.state::<RuntimeState>();
                    if !state.ready.load(Ordering::Acquire) {
                        return;
                    }
                    if state.shortcut_recording.load(Ordering::Acquire) {
                        return;
                    }
                    let Some(window) = app.get_webview_window("main") else {
                        return;
                    };
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.emit("shortcut-hide", ());
                        return;
                    }
                    let started = Instant::now();
                    let sequence = state.begin_summon(started);
                    let _ = window.show();
                    let _ = window.set_focus();
                    state.record_summon(started.elapsed().as_micros() as u64);
                    move_window_to_cursor_monitor(&window, &state);
                    let _ = window.emit("shortcut-show", sequence);
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            load_initial_state,
            mark_ready,
            set_shortcut_recording,
            save_note,
            update_draft,
            update_draft_view,
            navigate,
            neighbors,
            select_note,
            delete_note,
            restore_note,
            list_pages,
            list_deleted,
            set_shortcut,
            set_autostart,
            set_font_size,
            set_theme,
            set_glass_settings,
            set_panel,
            save_window_state,
            export_notes,
            hide_window,
            start_window_drag,
            quit_app,
            summon_metrics,
            record_visible_frame,
            record_first_input,
            list_attachments,
            paste_clipboard_image,
            start_visible_clipboard,
            stop_visible_clipboard,
            append_clipboard_change,
            clipboard_settings,
            set_clipboard_settings,
            ai_settings,
            set_ai_settings,
            detect_ai_providers,
            run_ai,
            cancel_ai,
            open_external,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?;
            let database = Database::open(data_dir)?;
            let shortcut = database.shortcut()?;
            let launch_at_login = database.launch_at_login()?;
            let theme = database.theme()?;
            let glass_settings = database.glass_settings()?;
            let _ = database.backup();
            app.manage(RuntimeState::new(database));

            if launch_at_login {
                let _ = app.autolaunch().enable();
            }
            app.global_shortcut().register(shortcut.as_str())?;

            let window = app
                .get_webview_window("main")
                .ok_or("main window missing")?;
            let state = app.state::<RuntimeState>();
            restore_window(&window, &state);
            configure_native_scratchpad_window(&window);
            let system_theme = window.theme().unwrap_or(Theme::Dark);
            apply_window_material(&window, &theme, system_theme, &glass_settings)?;
            window.set_always_on_top(true)?;
            window.set_maximizable(false)?;
            window.set_fullscreen(false)?;
            window.on_window_event({
                let window = window.clone();
                move |event| match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = window.emit("shortcut-hide", ());
                    }
                    WindowEvent::ThemeChanged(system_theme) => {
                        let state = window.state::<RuntimeState>();
                        let appearance = state
                            .db
                            .lock()
                            .ok()
                            .and_then(|db| Some((db.theme().ok()?, db.glass_settings().ok()?)));
                        if let Some((theme, glass_settings)) = appearance
                            && theme == "auto"
                        {
                            let _ = apply_window_material(
                                &window,
                                &theme,
                                *system_theme,
                                &glass_settings,
                            );
                        }
                    }
                    _ => {}
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build not");

    app.run(|handle, event| match event {
        RunEvent::ExitRequested { api, .. } => {
            let state = handle.state::<RuntimeState>();
            if !state.allow_exit.load(Ordering::Acquire) {
                api.prevent_exit();
                let _ = handle.emit("request-quit", ());
            }
        }
        RunEvent::Reopen { .. } => {
            let state = handle.state::<RuntimeState>();
            if state.ready.load(Ordering::Acquire)
                && let Some(window) = handle.get_webview_window("main")
            {
                let sequence = state.begin_summon(Instant::now());
                let _ = window.show();
                let _ = window.set_focus();
                move_window_to_cursor_monitor(&window, &state);
                let _ = window.emit("shortcut-show", sequence);
            }
        }
        RunEvent::Exit => {
            let state = handle.state::<RuntimeState>();
            if !state.exit_backup_completed.swap(true, Ordering::AcqRel)
                && let Ok(mut database) = state.db.lock()
            {
                if let Ok(draft) = state.draft.lock() {
                    let _ = persist_draft_and_backup(&mut database, draft.as_ref());
                } else {
                    let _ = database.backup();
                }
            }
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::{
        PanelSide, appkit_local_offset, choose_panel_side, expanded_panel_window,
        filtered_text_reason, ignored_application, insert_after_caret_line,
        insert_attachment_reference, looks_sensitive, persist_draft_and_backup,
        position_for_local_offset, position_for_placement, relative_placement, restored_geometry,
        safe_external_url,
    };
    use crate::db::{ClipboardSettings, Database, NoteInput};
    use uuid::Uuid;

    #[test]
    fn relative_window_placement_is_clamped_to_the_display() {
        assert_eq!(
            relative_placement(3000, -500, 440, 340, 0, 0, 1920, 1080),
            (1.0, 0.0)
        );
    }

    #[test]
    fn window_keeps_its_relative_position_on_another_monitor() {
        assert_eq!(
            relative_placement(750, 500, 200, 200, 0, 0, 1000, 800),
            (0.9375, 5.0 / 6.0)
        );
        let translated = position_for_placement(
            (0.9375, 5.0 / 6.0),
            200.0,
            200.0,
            1000.0,
            0.0,
            2000.0,
            1400.0,
        );
        assert!((translated.0 - 2687.5).abs() < 0.001);
        assert!((translated.1 - 200.0).abs() < 0.001);

        assert_eq!(
            restored_geometry((0.25, 0.75), (440.0, 340.0), 1.0, (0, 0, 1920, 1080)),
            (370, 555, 440, 340)
        );
        assert_eq!(
            restored_geometry((0.25, 0.75), (440.0, 340.0), 2.0, (3840, -1800, 3000, 1800),),
            (4370, -960, 880, 680)
        );
    }

    #[test]
    fn appkit_window_keeps_the_same_local_offset_on_another_monitor() {
        let offset = appkit_local_offset(2200.0, 500.0, 440.0, 340.0, 1920.0, 0.0, 1920.0, 1080.0);
        assert_eq!(offset, (280.0, 240.0));

        let translated =
            position_for_local_offset(offset, 440.0, 340.0, -1920.0, -200.0, 2560.0, 1440.0);
        assert!((translated.0 - -1640.0).abs() < 0.000_001);
        assert!((translated.1 - 660.0).abs() < 0.000_001);
    }

    #[test]
    fn local_monitor_offset_is_clamped_for_a_smaller_target() {
        assert_eq!(
            position_for_local_offset((1500.0, 900.0), 440.0, 340.0, 0.0, 0.0, 1280.0, 800.0,),
            (840.0, 0.0)
        );
    }

    #[test]
    fn panel_prefers_right_then_left_then_overlay() {
        assert_eq!(choose_panel_side(300, 500, 300), (PanelSide::Right, true));
        assert_eq!(choose_panel_side(299, 300, 300), (PanelSide::Left, true));
        assert_eq!(
            choose_panel_side(299, 299, 300),
            (PanelSide::Overlay, false)
        );
    }

    #[test]
    fn outward_panel_keeps_the_editor_at_the_same_screen_x() {
        let editor_x = 800;
        let editor_width = 440;
        let panel_width = 300;
        let (right_x, right_width) =
            expanded_panel_window(editor_x, editor_width, panel_width, PanelSide::Right);
        assert_eq!((right_x, right_width), (800, 740));

        let (left_x, left_width) =
            expanded_panel_window(editor_x, editor_width, panel_width, PanelSide::Left);
        assert_eq!((left_x, left_width), (500, 740));
        assert_eq!(left_x + panel_width as i32, editor_x);
    }

    #[test]
    fn native_exit_persists_the_latest_in_memory_draft() {
        let path = std::env::temp_dir().join(format!("not-exit-test-{}", Uuid::new_v4()));
        let note_id;
        {
            let mut database = Database::open(path.clone()).unwrap();
            let note = database.initial_note().unwrap();
            note_id = note.id.clone();
            let draft = NoteInput {
                id: note.id,
                body: "quit-flush-123".to_string(),
                position: note.position,
                created_at: note.created_at,
                cursor_start: 14,
                cursor_end: 14,
                scroll_top: 7.0,
                persisted: note.persisted,
            };
            persist_draft_and_backup(&mut database, Some(&draft)).unwrap();
        }

        let reopened = Database::open(path).unwrap();
        let restored = reopened.initial_note().unwrap();
        assert_eq!(restored.id, note_id);
        assert_eq!(restored.body, "quit-flush-123");
        assert_eq!(restored.scroll_top, 7.0);
    }

    #[test]
    fn clipboard_filters_cover_size_whitespace_secrets_and_ignored_apps() {
        let settings = ClipboardSettings::default();
        assert!(filtered_text_reason("   ", &settings).is_some());
        assert!(looks_sensitive("api_key=definitely-secret"));
        assert!(filtered_text_reason("api_key=definitely-secret", &settings).is_some());

        let settings = ClipboardSettings {
            minimum_text_length: 5,
            maximum_text_length: 8,
            ignored_applications: "com.password.manager,\ncom.private.app".to_string(),
            ..settings
        };
        assert!(filtered_text_reason("tiny", &settings).is_some());
        assert!(filtered_text_reason("this is too long", &settings).is_some());
        assert!(filtered_text_reason("allowed", &settings).is_none());
        assert!(ignored_application(&settings, Some("com.private.app")));
        assert!(!ignored_application(&settings, Some("com.apple.TextEdit")));
    }

    #[test]
    fn external_links_allow_only_validated_user_initiated_schemes() {
        assert!(safe_external_url("https://example.com/note"));
        assert!(safe_external_url("mailto:person@example.com"));
        assert!(!safe_external_url("javascript:alert(1)"));
        assert!(!safe_external_url("https://example.com/bad path"));
    }

    #[test]
    fn image_reference_replaces_utf16_selection_inline_without_splitting_unicode() {
        let mut input = NoteInput {
            id: "note".to_string(),
            body: "a😀selected end".to_string(),
            position: 1,
            created_at: 1,
            cursor_start: 3,
            cursor_end: 12,
            scroll_top: 0.0,
            persisted: true,
        };
        insert_attachment_reference(&mut input, "image-id");
        assert_eq!(
            input.body,
            "a😀![clipboard image](attachment://image-id)end"
        );
        assert_eq!(input.cursor_start, input.cursor_end);
    }

    #[test]
    fn visible_clipboard_append_inserts_after_the_remembered_caret_line() {
        let mut input = NoteInput {
            id: "note".to_string(),
            body: "first line\nlast line".to_string(),
            position: 1,
            created_at: 1,
            cursor_start: 2,
            cursor_end: 2,
            scroll_top: 0.0,
            persisted: true,
        };
        insert_after_caret_line(&mut input, 10, "copied text");
        assert_eq!(input.body, "first line\ncopied text\nlast line");
        assert_eq!((input.cursor_start, input.cursor_end), (2, 2));
    }
}
