mod db;

use db::{Database, DeletedPage, Neighbors, Note, NoteInput, PageSummary, SaveResult};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
use tauri::{
    AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, RunEvent, State,
    WebviewWindow, WindowEvent,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutostartExt};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

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
    launch_at_login: bool,
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

#[tauri::command]
async fn load_initial_state(
    app: AppHandle,
    state: State<'_, RuntimeState>,
) -> Result<InitialState, String> {
    let (note, shortcut, launch_at_login) = database_task(&state, |db| {
        Ok((db.initial_note()?, db.shortcut()?, db.launch_at_login()?))
    })
    .await?;
    let actual_autostart = app.autolaunch().is_enabled().unwrap_or(false);
    Ok(InitialState {
        note,
        shortcut,
        launch_at_login: launch_at_login && actual_autostart,
    })
}

#[tauri::command]
fn mark_ready(state: State<'_, RuntimeState>) {
    state.ready.store(true, Ordering::Release);
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
    database_task(&state, move |db| db.set_shortcut(&saved)).await?;
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
fn set_panel(
    open: bool,
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
    let panel_width = (PANEL_WIDTH_LOGICAL * scale).round() as u32;
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let right_space = i64::from(monitor_position.x) + i64::from(monitor_size.width)
        - i64::from(position.x)
        - i64::from(size.width);
    let left_space = i64::from(position.x) - i64::from(monitor_position.x);

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
        window
            .set_resizable(false)
            .map_err(|error| error.to_string())?;
        window
            .set_size(PhysicalSize::new(size.width + panel_width, size.height))
            .map_err(|error| error.to_string())?;
        if matches!(side, PanelSide::Left) {
            window
                .set_position(PhysicalPosition::new(
                    position.x - panel_width as i32,
                    position.y,
                ))
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
    let display_identifier = window
        .current_monitor()
        .map_err(|error| error.to_string())?
        .map(|monitor| monitor_identifier(&monitor));
    database_task(&state, move |db| {
        db.save_window_state(
            position.x,
            position.y,
            size.width,
            size.height,
            scale,
            display_identifier,
        )
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
    let ratio = current_scale / saved.scale_factor.max(0.1);
    let width = (f64::from(saved.width) * ratio).round() as u32;
    let height = (f64::from(saved.height) * ratio).round() as u32;
    let (x, y) = target
        .map(|monitor| {
            let area = monitor.work_area();
            clamp_rect_to_bounds(
                saved.x,
                saved.y,
                width,
                height,
                area.position.x,
                area.position.y,
                area.size.width,
                area.size.height,
            )
        })
        .unwrap_or((saved.x, saved.y));
    let _ = window.set_size(PhysicalSize::new(width, height));
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn monitor_identifier(monitor: &Monitor) -> String {
    monitor.name().cloned().unwrap_or_else(|| {
        let position = monitor.position();
        format!("{}:{}", position.x, position.y)
    })
}

fn clamp_rect_to_bounds(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    bounds_x: i32,
    bounds_y: i32,
    bounds_width: u32,
    bounds_height: u32,
) -> (i32, i32) {
    let max_x = bounds_x + bounds_width.saturating_sub(width) as i32;
    let max_y = bounds_y + bounds_height.saturating_sub(height) as i32;
    (x.clamp(bounds_x, max_x), y.clamp(bounds_y, max_y))
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

pub fn run() {
    let app = tauri::Builder::default()
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
                    let _ = window.emit("shortcut-show", sequence);
                    state.record_summon(started.elapsed().as_micros() as u64);
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            load_initial_state,
            mark_ready,
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
            set_panel,
            save_window_state,
            export_notes,
            hide_window,
            start_window_drag,
            quit_app,
            summon_metrics,
            record_visible_frame,
            record_first_input,
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
            window.set_always_on_top(true)?;
            window.set_maximizable(false)?;
            window.set_fullscreen(false)?;
            window.on_window_event({
                let window = window.clone();
                move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window.emit("shortcut-hide", ());
                    }
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
                let _ = window.show();
                let _ = window.set_focus();
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
    use super::{PanelSide, choose_panel_side, clamp_rect_to_bounds, persist_draft_and_backup};
    use crate::db::{Database, NoteInput};
    use uuid::Uuid;

    #[test]
    fn restored_window_is_clamped_to_available_display() {
        assert_eq!(
            clamp_rect_to_bounds(3000, -500, 440, 340, 0, 0, 1920, 1080),
            (1480, 0)
        );
        assert_eq!(
            clamp_rect_to_bounds(-2000, 2000, 440, 340, -1920, 0, 1920, 1080),
            (-1920, 740)
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
}
