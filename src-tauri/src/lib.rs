mod db;

use db::{Database, DeletedPage, Neighbors, Note, NoteInput, PageSummary, SaveResult};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, RunEvent, State, WebviewWindow,
    WindowEvent,
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

#[derive(Clone, Copy, Serialize)]
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
}

impl RuntimeState {
    fn new(db: Database) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            ready: AtomicBool::new(false),
            allow_exit: AtomicBool::new(false),
            panel: Mutex::new(None),
            metrics: Mutex::new(VecDeque::with_capacity(256)),
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

    let (side, external) = if right_space >= i64::from(panel_width) {
        (PanelSide::Right, true)
    } else if left_space >= i64::from(panel_width) {
        (PanelSide::Left, true)
    } else {
        (PanelSide::Overlay, false)
    };
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
    database_task(&state, move |db| {
        db.save_window_state(position.x, position.y, size.width, size.height, scale)
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
fn quit_app(app: AppHandle, state: State<'_, RuntimeState>) {
    state.allow_exit.store(true, Ordering::Release);
    app.exit(0);
}

#[tauri::command]
fn summon_metrics(state: State<'_, RuntimeState>) -> SummonMetrics {
    let mut values = state
        .metrics
        .lock()
        .map(|items| items.iter().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    values.sort_unstable();
    SummonMetrics {
        count: values.len(),
        p50_micros: percentile(&values, 0.50),
        p95_micros: percentile(&values, 0.95),
        p99_micros: percentile(&values, 0.99),
    }
}

fn percentile(values: &[u64], percentile: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let index = ((values.len() - 1) as f64 * percentile).round() as usize;
    values[index]
}

fn restore_window(window: &WebviewWindow, state: &RuntimeState) {
    let Ok(database) = state.db.lock() else {
        return;
    };
    let Ok(Some(saved)) = database.window_state() else {
        return;
    };
    let current_scale = window.scale_factor().unwrap_or(saved.scale_factor);
    let ratio = current_scale / saved.scale_factor;
    let _ = window.set_size(PhysicalSize::new(
        (f64::from(saved.width) * ratio).round() as u32,
        (f64::from(saved.height) * ratio).round() as u32,
    ));
    let _ = window.set_position(PhysicalPosition::new(saved.x, saved.y));
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
                    let _ = window.show();
                    let _ = window.set_focus();
                    state.record_summon(started.elapsed().as_micros() as u64);
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            load_initial_state,
            mark_ready,
            save_note,
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
        _ => {}
    });
}
