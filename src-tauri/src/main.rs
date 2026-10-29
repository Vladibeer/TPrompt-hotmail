// Без конзолен прозорец в готовата програма на Windows
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod geometry;
mod remote;

use geometry::{Place, Rect, Saved};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Position, Size, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Window, WindowEvent,
};

const REMOTE_HTML: &str = include_str!("../remote.html");

type Remote = Arc<remote::Remote>;

fn info_json(i: remote::Info) -> serde_json::Value {
    serde_json::json!({ "ip": i.ip, "port": i.port, "token": i.token, "url": i.url })
}

#[tauri::command]
fn remote_start(state: State<'_, Remote>, token: Option<String>) -> Result<serde_json::Value, String> {
    // запазеният код се връща само преди пускане на сървъра
    if let Some(t) = token {
        if !state.is_running() {
            state.set_token(&t);
        }
    }
    state.start().map(info_json)
}

#[tauri::command]
fn remote_stop(state: State<'_, Remote>) {
    state.stop();
}

#[tauri::command]
fn remote_new_token(state: State<'_, Remote>) -> Result<serde_json::Value, String> {
    state.new_token();
    state
        .info()
        .map(info_json)
        .ok_or_else(|| "Дистанционното не е включено.".to_string())
}

#[tauri::command]
fn remote_sync(state: State<'_, Remote>, state_json: String) -> Vec<String> {
    state.sync(state_json)
}

#[tauri::command]
fn remote_set_content(state: State<'_, Remote>, content_json: String) {
    state.set_content(content_json);
}

// Приложението на телефона праща съобщения през мрежата; тук ги взема главният прозорец.
// Едновременно публикува състоянието на дисплея, което телефонът чете.
#[tauri::command]
fn rx_pump(state: State<'_, Remote>, rx_state: String) -> serde_json::Value {
    if !rx_state.is_empty() {
        state.set_rx_state(rx_state);
    }
    let (rx_ago, msgs) = state.rx_drain();
    let st = state.status();
    serde_json::json!({
        "running": st.running,
        "rxAgo": rx_ago.or(st.rx_ago_ms),
        "webAgo": st.web_ago_ms,
        "msgs": msgs,
    })
}

// Компютърът праща съобщение към приложението на телефона (телефонът чете, компютърът управлява)
#[tauri::command]
fn tx_push(state: State<'_, Remote>, msg_json: String) {
    state.push_tx(msg_json);
}

#[tauri::command]
fn tx_clear(state: State<'_, Remote>) {
    state.clear_tx();
}

/// Компютърът моли приложението на телефона да започне да чете (view = true) или да спре.
#[tauri::command]
fn remote_want(state: State<'_, Remote>, view: bool) {
    state.set_want(view);
}

#[tauri::command]
fn monitor_count(app: AppHandle) -> usize {
    app.available_monitors().map(|m| m.len()).unwrap_or(1)
}

#[tauri::command]
fn reader_open(app: AppHandle) -> bool {
    app.get_webview_window("reader").is_some()
}

#[tauri::command]
async fn close_reader(app: AppHandle) {
    if let Some(w) = app.get_webview_window("reader") {
        let _ = w.destroy();
    }
}

// ---------- позиция и размер на прозорците ----------

const READER_DEFAULT: (u32, u32) = (1280, 720);

struct GeoInner {
    map: HashMap<String, Saved>,
    last_write: Instant,
    dirty: bool,
}

struct Geo {
    path: Option<PathBuf>,
    inner: Mutex<GeoInner>,
}

impl Geo {
    fn load(app: &AppHandle) -> Geo {
        let path = app.path().app_config_dir().ok().map(|d| d.join("windows.json"));
        let mut map = HashMap::new();
        if let Some(p) = &path {
            if let Ok(text) = std::fs::read_to_string(p) {
                for k in ["main", "reader"] {
                    if let Some(sv) = geometry::file_get(&text, k) {
                        map.insert(k.to_string(), sv);
                    }
                }
            }
        }
        Geo {
            path,
            inner: Mutex::new(GeoInner { map, last_write: Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(Instant::now), dirty: false }),
        }
    }

    fn get(&self, label: &str) -> Option<Saved> {
        self.inner.lock().ok()?.map.get(label).copied()
    }

    fn flush_locked(&self, g: &mut GeoInner) {
        if let Some(p) = &self.path {
            let entries: Vec<(&str, Saved)> = ["main", "reader"]
                .iter()
                .filter_map(|k| g.map.get(*k).map(|v| (*k, *v)))
                .collect();
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, geometry::file_to_json(&entries));
        }
        g.last_write = Instant::now();
        g.dirty = false;
    }

    fn flush(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if g.dirty {
                self.flush_locked(&mut g);
            }
        }
    }

    fn update(&self, label: &str, sv: Saved, force: bool) {
        if let Ok(mut g) = self.inner.lock() {
            if g.map.get(label) != Some(&sv) {
                g.map.insert(label.to_string(), sv);
                g.dirty = true;
            }
            if g.dirty && (force || g.last_write.elapsed() > Duration::from_millis(700)) {
                self.flush_locked(&mut g);
            }
        }
    }
}

fn monitor_rect(m: &tauri::Monitor) -> Rect {
    let p = m.position();
    let s = m.size();
    Rect::new(p.x, p.y, s.width, s.height)
}

fn all_monitors(app: &AppHandle) -> Vec<Rect> {
    app.available_monitors().map(|v| v.iter().map(monitor_rect).collect()).unwrap_or_default()
}

fn window_monitor(w: &Window) -> Option<Rect> {
    w.current_monitor().ok().flatten().map(|m| monitor_rect(&m))
}

// Запомня текущото състояние на прозореца (не пипа размера, докато е на цял екран/увеличен/свит)
fn remember(window: &Window, force: bool) {
    let label = window.label().to_string();
    if label != "main" && label != "reader" {
        return;
    }
    if window.is_minimized().unwrap_or(false) {
        return;
    }
    let app = window.app_handle();
    // събития могат да дойдат, преди да е готово състоянието
    let Some(geo) = app.try_state::<Arc<Geo>>() else { return };
    let fullscreen = window.is_fullscreen().unwrap_or(false);
    let maximized = window.is_maximized().unwrap_or(false);
    let monitor = window_monitor(window);
    let prev = geo.get(&label);
    let rect = if fullscreen || maximized {
        match prev {
            Some(p) => p.rect,
            None => match (window.outer_position(), window.inner_size()) {
                (Ok(p), Ok(s)) => Rect::new(p.x, p.y, s.width, s.height),
                _ => return,
            },
        }
    } else {
        match (window.outer_position(), window.inner_size()) {
            (Ok(p), Ok(s)) if s.width > 0 && s.height > 0 => Rect::new(p.x, p.y, s.width, s.height),
            _ => return,
        }
    };
    geo.update(&label, Saved { rect, maximized, fullscreen, monitor }, force);
}

fn place_window(w: &WebviewWindow, place: &Place) {
    if let Some(r) = place.rect {
        let _ = w.set_size(Size::Physical(PhysicalSize::new(r.w, r.h)));
        let _ = w.set_position(Position::Physical(PhysicalPosition::new(r.x, r.y)));
    }
    if place.maximized {
        let _ = w.maximize();
    }
    if place.fullscreen {
        let _ = w.set_fullscreen(true);
    }
}

fn on_window_event(window: &Window, event: &WindowEvent) {
    match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => remember(window, false),
        WindowEvent::CloseRequested { .. } => {
            remember(window, true);
            if window.label() == "main" {
                // със затварянето на главния прозорец се затваря и вторият, за да излезе програмата
                if let Some(g) = window.app_handle().try_state::<Arc<Geo>>() {
                    g.flush();
                }
                if let Some(r) = window.app_handle().get_webview_window("reader") {
                    let _ = r.destroy();
                }
            }
        }
        WindowEvent::Destroyed => {
            if let Some(g) = window.app_handle().try_state::<Arc<Geo>>() {
                g.flush();
            }
        }
        _ => {}
    }
}

// Втори прозорец за четене: на другия монитор на цял екран; ако няма такъв, на основния.
// Помни последната позиция и размер.
#[tauri::command]
async fn open_reader(app: AppHandle, geo: State<'_, Arc<Geo>>) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("reader") {
        let _ = w.set_focus();
        return Ok(());
    }
    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "Няма главен прозорец.".to_string())?;
    let mons = all_monitors(&app);
    let main_mon = main
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| monitor_rect(&m))
        .or_else(|| mons.first().copied())
        .ok_or_else(|| "Не е намерен монитор.".to_string())?;
    let saved = geo.get("reader");
    let place = geometry::plan_reader(saved.as_ref(), &mons, &main_mon, READER_DEFAULT);

    let w = WebviewWindowBuilder::new(&app, "reader", WebviewUrl::App("index.html".into()))
        .title("TPrompt")
        .inner_size(READER_DEFAULT.0 as f64, READER_DEFAULT.1 as f64)
        .visible(false)
        .initialization_script("window.__TP_VIEWER = true;")
        .build()
        .map_err(|e| e.to_string())?;
    place_window(&w, &place);
    let _ = w.show();
    let _ = w.set_focus();
    Ok(())
}

// ---------- старт с Windows (минимизирано) ----------
// Записва се в HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run; без допълнителни пакети.
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "TPrompt";

#[cfg(windows)]
fn reg(args: &[&str]) -> Option<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg").args(args).creation_flags(0x0800_0000).output().ok()
}

#[tauri::command]
fn autostart_get() -> bool {
    #[cfg(windows)]
    {
        return reg(&["query", RUN_KEY, "/v", RUN_NAME]).map(|o| o.status.success()).unwrap_or(false);
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[tauri::command]
fn autostart_set(enabled: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let val = format!("\"{}\" --minimized", exe.display());
            let o = reg(&["add", RUN_KEY, "/v", RUN_NAME, "/t", "REG_SZ", "/d", &val, "/f"]).ok_or("Не успях да запиша настройката.")?;
            if !o.status.success() {
                return Err("Не успях да запиша настройката.".to_string());
            }
        } else {
            // липсващ запис не е грешка
            let _ = reg(&["delete", RUN_KEY, "/v", RUN_NAME, "/f"]);
        }
        return Ok(autostart_get() == enabled);
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err("Само за Windows.".to_string())
    }
}

// Запазване на файл с обикновен диалог „Запази като“; връща пътя или null при отказ
#[tauri::command]
async fn save_file(name: String, data: String) -> Result<Option<String>, String> {
    let picked = rfd::AsyncFileDialog::new()
        .set_file_name(&name)
        .save_file()
        .await;
    match picked {
        Some(h) => {
            let path = h.path().to_path_buf();
            std::fs::write(&path, data.as_bytes()).map_err(|e| e.to_string())?;
            Ok(Some(path.to_string_lossy().into_owned()))
        }
        None => Ok(None),
    }
}

// Настройки на WebView2 срещу „желе“ и накъсване при два прозореца.
// Chromium сам решава, че прозорец, който е закрит от друг или не е на фокус, е „скрит“, и му забавя
// таймерите и кадрите, после го събужда рязко: точно това се вижда като накъсване. Тук го изключваме.
// Променливата се добавя към аргументите, които Tauri вече задава; --disable-features се повтаря заедно
// със стойностите по подразбиране, защото се взема само последният такъв аргумент.
#[cfg(windows)]
fn webview_tuning() {
    if std::env::var_os("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_some() {
        return;
    }
    std::env::set_var(
        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion,UseEcoQoSForBackgroundProcess \
         --disable-renderer-backgrounding \
         --disable-background-timer-throttling \
         --disable-backgrounding-occluded-windows",
    );
}

fn main() {
    #[cfg(windows)]
    webview_tuning();
    let remote: Remote = remote::Remote::new(REMOTE_HTML);
    tauri::Builder::default()
        .manage(remote)
        .setup(|app| {
            let handle = app.handle().clone();
            let geo = Arc::new(Geo::load(&handle));
            app.manage(geo.clone());
            if let Some(w) = app.get_webview_window("main") {
                let mons = all_monitors(&handle);
                if let Some(place) = geometry::plan_main(geo.get("main").as_ref(), &mons) {
                    place_window(&w, &place);
                }
                let _ = w.show();
                // старт с Windows: прозорецът остава минимизиран
                if std::env::args().any(|a| a == "--minimized") {
                    let _ = w.minimize();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| on_window_event(window, event))
        .invoke_handler(tauri::generate_handler![
            remote_start,
            remote_stop,
            remote_new_token,
            remote_sync,
            remote_set_content,
            rx_pump,
            tx_push,
            tx_clear,
            remote_want,
            monitor_count,
            reader_open,
            close_reader,
            open_reader,
            save_file,
            autostart_get,
            autostart_set
        ])
        .run(tauri::generate_context!())
        .expect("грешка при стартиране на TPrompt");
}
