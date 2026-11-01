/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! `swiss gui` — the panel in its own window (SPEC §host.gui, ADR-038).
//!
//! The gateway already is the whole product: it serves the panel over loopback and mints a
//! one-time sign-in link for it (`swiss open`). This module adds only the missing window: a
//! tao message loop, a wry WebView2 surface pointed at that same sign-in link, and a tray to
//! keep it reachable. No framework, no bundler, no second engine — on Windows the system
//! WebView2 or macOS WKWebView renders the very bytes a browser tab already renders. Linux
//! stays headless (ADR-038: the gateway serves, the OS supplies the window's browser engine).
//!
//! The split is deliberate: `launch_sequence` is platform-neutral and tested like the rest of
//! the CLI; `run` is the native event loop, which no unit test can drive and is proven live
//! instead (SPEC §testing.live).

use crate::daemon::{self, StartOptions, StartResult};

#[cfg(any(windows, target_os = "macos"))]
mod desktop;

#[cfg(any(target_os = "macos", all(test, unix)))]
mod unix_instance;

// The shell's window stack (ADR-038). tao's loop and window, the system webview and tray —
// three small crates, no framework between them
// and the message loop.
#[cfg(any(windows, target_os = "macos"))]
use tao::event::{Event, WindowEvent};
#[cfg(any(windows, target_os = "macos"))]
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder};
#[cfg(any(windows, target_os = "macos"))]
use tao::window::{Icon as WindowIcon, Theme, WindowBuilder};
#[cfg(any(windows, target_os = "macos"))]
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
#[cfg(any(windows, target_os = "macos"))]
use tray_icon::{Icon as TrayIconPixels, TrayIconBuilder, TrayIconEvent};
#[cfg(any(windows, target_os = "macos"))]
use wry::dpi::{PhysicalPosition, PhysicalSize, Position, Size as WebViewSize};
#[cfg(any(windows, target_os = "macos"))]
use wry::{NewWindowResponse, Rect, WebContext, WebViewBuilder};

/// A standing window that can no longer paint is worse than none: the next `swiss gui` only
/// raises it. This is how long that next launch waits for the zombie to exit and hand the
/// identity over — ten tries, a fifth of a second apart.
#[cfg(any(windows, target_os = "macos"))]
const CLAIM_TAKEOVER_TRIES: u32 = 10;
#[cfg(any(windows, target_os = "macos"))]
const CLAIM_TAKEOVER_STEP: std::time::Duration = std::time::Duration::from_millis(200);

/// Bring the gateway up if it is not running, then mint the one-time sign-in link the window
/// spends. `start_daemon` is idempotent — a running gateway answers `AlreadyRunning` without a
/// second spawn — so this is safe to run on every `swiss gui`, exactly like `swiss start`.
pub async fn launch_sequence(opts: StartOptions) -> Result<String, String> {
    match daemon::start_daemon(opts).await {
        StartResult::Started { port, .. } | StartResult::AlreadyRunning { port, .. } => {
            daemon::login_url(port).await
        }
        StartResult::Failed { log_tail, .. } => {
            Err(format!("the gateway did not come up:\n{log_tail}"))
        }
    }
}

/// The explicit `swiss gui` launcher: re-enter this binary in its foreground window mode,
/// detached from the terminal exactly as `swiss start` detaches `serve`. The child owns the
/// native event loop; this process reports the spawn and returns instead of waiting for Exit.
#[cfg(any(windows, target_os = "macos"))]
pub fn launch(port: Option<u16>) -> i32 {
    let port = port.unwrap_or_else(daemon::resolve_port);
    // The child must not inherit a wrapper's output pipe (that is the hang this launcher
    // removes), yet a window that dies before its event loop paints still has to say why - a
    // macOS failure would otherwise be entirely silent, and a double-clicked app has no
    // terminal at all. Both streams append to the home's `gui-<port>.log` (SPEC §host.gui).
    let (stdout, stderr) = window_stdio(port);
    let mut command = std::process::Command::new(daemon::server_entry());
    command
        .args(foreground_args(port))
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        // Null stdio is not enough on Windows: an inherited pipe would keep a wrapper waiting
        // for EOF until the window exited, recreating the very hang this launcher avoids.
        swiss_core::platform::keep_std_handles_from_children();
    }
    #[cfg(target_os = "macos")]
    swiss_core::platform::detach_command(&mut command);
    match command.spawn() {
        Ok(child) => {
            println!("swiss gui launched (pid {})", child.id());
            println!("url {}", daemon::url_for(port));
            0
        }
        Err(error) => {
            eprintln!("swiss gui: could not launch the window: {error}");
            1
        }
    }
}

/// The detached window's streams: append both to `gui-<port>.log` in the data home, one
/// handle each. A log that cannot be opened falls back to null — the pre-log behavior.
#[cfg(any(windows, target_os = "macos"))]
fn window_stdio(port: u16) -> (std::process::Stdio, std::process::Stdio) {
    let open = || -> Option<std::process::Stdio> {
        let dir = swiss_core::paths::data_dir();
        std::fs::create_dir_all(&dir).ok()?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("gui-{port}.log")))
            .ok()?;
        Some(std::process::Stdio::from(log))
    };
    match (open(), open()) {
        (Some(out), Some(err)) => (out, err),
        _ => (std::process::Stdio::null(), std::process::Stdio::null()),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn launch(port: Option<u16>) -> i32 {
    run(port)
}

/// The detached launch must name foreground mode explicitly: without this flag its child
/// would launch another child and exit, and no process would remain to own the window.
#[cfg(any(windows, target_os = "macos", test))]
fn foreground_args(port: u16) -> Vec<String> {
    vec!["gui".to_string(), "--foreground".to_string(), "--port".to_string(), port.to_string()]
}

/// The Windows entry point and its named-kernel-object instance claim (SPEC §host.gui).
#[cfg(windows)]
pub fn run(port: Option<u16>) -> i32 {
    desktop::detach_console();
    // The window belongs to ONE gateway - this home on this port, resolved exactly the way
    // the starter below resolves it (env `SWISS_PORT`, then the home's config, then 19999).
    // A desktop-wide single instance would weld every home to whichever window opened first
    // and swallow an operator's config switch whole (2026-10-30: a second `swiss gui` under
    // another SWISS_HOME exited silently while the first window, hidden in the tray, kept
    // showing the old gateway's panel). One window per gateway; two gateways, two windows.
    let port = port.unwrap_or_else(daemon::resolve_port);
    // One window per gateway. A standing window for this identity is raised, not duplicated —
    // unless its gateway is gone (the panel's Quit, or `swiss stop` from a terminal): that shell
    // can never paint again, and it holds the claim, so this launch waits for it to leave and
    // takes the identity over. Without that wait the only exit from a dead window was the tray.
    let id = instance_id(port, &swiss_core::paths::data_dir());
    let mut waits = 0;
    loop {
        match claim_instance(&id) {
            Instance::Claimed { show } => return shell(port, show),
            Instance::AlreadyRunning => {
                if waits >= CLAIM_TAKEOVER_TRIES || daemon::port_accepts(port) {
                    eprintln!("a swiss window for this home is already open - bringing it to the front");
                    return 0;
                }
                waits += 1;
                std::thread::sleep(CLAIM_TAKEOVER_STEP);
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn run(port: Option<u16>) -> i32 {
    let port = port.unwrap_or_else(daemon::resolve_port);
    let home = swiss_core::paths::data_dir();
    if let Err(error) = std::fs::create_dir_all(&home) {
        desktop::startup_error(&format!("cannot open the application home: {error}"));
        return 1;
    }
    swiss_core::platform::mkdir_private(&home);
    let mut waits = 0;
    loop {
        match unix_instance::claim(&home, &instance_id(port, &home)) {
            Ok(unix_instance::Claim::Owned(signal)) => return shell(port, signal),
            // Same takeover as the Windows entry: a zombie window for a dead gateway frees the
            // claim by exiting, and this launch waits it out instead of reporting "already open".
            Ok(unix_instance::Claim::Existing) if waits < CLAIM_TAKEOVER_TRIES && !daemon::port_accepts(port) => {
                waits += 1;
                std::thread::sleep(CLAIM_TAKEOVER_STEP);
            }
            Ok(unix_instance::Claim::Existing) => return 0,
            Err(error) => { desktop::startup_error(&error.to_string()); return 1; }
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn run(_port: Option<u16>) -> i32 {
    eprintln!(
        "the window shell supports Windows and macOS; on this machine run `swiss start` and open the panel in a browser"
    );
    2
}

/// The desktop-session claim on one gateway identity: a named mutex this process holds for
/// its whole life, plus a named event the NEXT launcher of the same gateway signals so the
/// standing window shows itself — closing hides to the tray, so "already open" must never
/// look like "did nothing". A won claim comes back as the event handle to watch; losing the
/// race is AlreadyRunning, after the winner has been nudged.
///
/// SAFETY: platform FFI with names built only from a port and a path hash. The mutex handle
/// is intentionally never closed — closing it would release the single-instance claim the
/// process still stands on — and the event handle lives until the process does, waited on
/// by the shell's own thread.
#[cfg(windows)]
enum Instance {
    Claimed {
        show: windows::Win32::Foundation::HANDLE,
    },
    AlreadyRunning,
}

#[cfg(windows)]
fn claim_instance(id: &str) -> Instance {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent};
    let wide = |text: &str| -> Vec<u16> { format!("{text}\0").encode_utf16().collect() };
    // The event is created first: the loser of the mutex race then finds the winner's event
    // by name and can signal it. Auto-reset latches the signal until the winner first waits.
    let show = unsafe {
        CreateEventW(
            None,
            false,
            false,
            PCWSTR(wide(&format!("Local\\swiss-gui-{id}-show")).as_ptr()),
        )
    };
    let mutex = unsafe {
        CreateMutexW(
            None,
            false,
            PCWSTR(wide(&format!("Local\\swiss-gui-{id}")).as_ptr()),
        )
    };
    // CreateMutexW reports "a mutex by that name already existed" through last error while
    // still handing back a valid handle - windows-rs surfaces that as Ok, so the race is read
    // straight off GetLastError, immediately, before anything else can clobber it.
    let last_error = || unsafe { windows::Win32::Foundation::GetLastError() };
    let claimed = match mutex {
        Ok(_) => last_error() != ERROR_ALREADY_EXISTS,
        Err(err) if err.code() == ERROR_ALREADY_EXISTS.to_hresult() => false,
        // An error that is not "already exists" says nothing either way. Refusing to start on
        // it would hold the panel hostage to an OS bookkeeping hiccup; the mutex is a nicety,
        // not a boundary, so this instance runs.
        Err(_) => true,
    };
    if !claimed {
        if let Ok(show) = show {
            let _ = unsafe { SetEvent(show) };
            let _ = unsafe { CloseHandle(show) };
        }
        return Instance::AlreadyRunning;
    }
    // A failed event only costs the nudge, never the window.
    Instance::Claimed {
        show: show.unwrap_or(HANDLE::default()),
    }
}

/// The id of one gateway identity: its port and its home, the latter folded to a stable
/// FNV-1a word so a kernel object name never carries a user path. Case-folded like the
/// filesystem below it, so `C:\A` and `c:\a` are one gateway, one window.
#[cfg(any(windows, target_os = "macos"))]
fn instance_id(port: u16, home: &std::path::Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let path = home.to_string_lossy();
    let path = if cfg!(windows) { path.to_lowercase() } else { path.into_owned() };
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{port:x}-{hash:016x}")
}

/// The URL a new-window request may actually open, or None for everything else.
///
/// Only the two schemes a panel link may legitimately carry get through: the panel renders model
/// output and database cells, so `file:` (read the disk through the opener) and `javascript:`
/// (the opener is a shell, not a sandbox) are exactly what must not.
#[cfg(any(test, windows, target_os = "macos"))]
fn external_url(uri: &str) -> Option<String> {
    let url = reqwest::Url::parse(uri).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.as_str().to_string())
}

#[cfg(any(windows, target_os = "macos"))]
fn open_external(uri: &str) {
    if let Some(url) = external_url(uri) {
        crate::cli::open_in_browser(&url);
    }
}

#[cfg(any(test, windows, target_os = "macos"))]
fn panel_url(uri: &str, port: u16) -> bool {
    reqwest::Url::parse(uri).is_ok_and(|url| url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1") && url.port_or_known_default() == Some(port)
        && url.username().is_empty() && url.password().is_none())
}

#[cfg(any(test, windows, target_os = "macos"))]
fn navigation_allowed(uri: &str, port: u16, panel_started: &std::cell::Cell<bool>) -> bool {
    if panel_url(uri, port) { panel_started.set(true); return true; }
    uri == "about:blank" && !panel_started.get()
}

#[cfg(any(test, windows, target_os = "macos"))]
fn download_url(uri: &str, port: u16) -> bool {
    panel_url(uri.strip_prefix("blob:").unwrap_or(uri), port)
}

/// The window, the webview, the tray and the loop that keeps them alive. Never returns while
/// the app runs — tao's loop only unwinds into process exit — so every exit below is explicit.
#[cfg(any(windows, target_os = "macos"))]
fn shell(
    port: u16,
    #[cfg(windows)] show: windows::Win32::Foundation::HANDLE,
    #[cfg(target_os = "macos")] signal: unix_instance::Guard,
) -> i32 {
    // What reaches the loop from outside it: the sign-in link (from the starter thread), tray
    // and menu picks (from the global channels), and the quit.
    enum GuiEvent {
        PanelReady(String),
        Fatal(String),
        DownloadFinished(bool),
        TrayOpen,
        Desktop(desktop::Message),
        Quit,
    }

    let event_loop: EventLoop<GuiEvent> = EventLoopBuilder::with_user_event().build();
    #[cfg(target_os = "macos")]
    let _application_menu = {
        use tray_icon::menu::Submenu;
        let bar = Menu::new();
        let app = Submenu::new("swiss", true);
        let edit = Submenu::new("Edit", true);
        if let Err(error) = app.append_items(&[
            &PredefinedMenuItem::about(Some("About swiss"), None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Hide swiss")),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(Some("Quit swiss")),
        ]).and_then(|_| edit.append_items(&[
            &PredefinedMenuItem::undo(None), &PredefinedMenuItem::redo(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(None), &PredefinedMenuItem::copy(None),
            &PredefinedMenuItem::paste(None), &PredefinedMenuItem::select_all(None),
        ])).and_then(|_| bar.append_items(&[&app, &edit])) {
            desktop::startup_error(&format!("cannot create the application menu: {error}"));
            return 1;
        }
        bar.init_for_nsapp();
        bar
    };
    let prefs_path = swiss_core::paths::data_dir().join(format!("gui-window-{port}.json"));
    let mut prefs = desktop::Preferences::load(&prefs_path);
    let primary = event_loop.primary_monitor().map(|m| m.position());
    let screens: Vec<_> = event_loop.available_monitors().map(|m| {
        let p = m.position(); let s = m.size();
        desktop::Screen { position: (p.x, p.y), size: (s.width, s.height), scale: m.scale_factor(), primary: Some(p) == primary }
    }).collect();
    prefs.fit(&screens);

    // One icon, two owners: the window's title-bar icon and the tray's — and both carry the
    // brand mark itself (assets/logo.svg, SPEC §release), drawn by brand_icon() from the
    // SVG's own geometry: the red rounded tile, the fanned tools, the rivet.
    let (rgba, width, height) = brand_icon();
    let window = match WindowBuilder::new()
        .with_title("swiss")
        .with_inner_size(tao::dpi::LogicalSize::new(prefs.width, prefs.height))
        .with_min_inner_size(tao::dpi::LogicalSize::new(desktop::MIN_WIDTH, desktop::MIN_HEIGHT))
        .with_theme(prefs.dark.map(|dark| if dark { Theme::Dark } else { Theme::Light }))
        .with_visible(false)
        .with_window_icon(WindowIcon::from_rgba(rgba.clone(), width, height).ok())
        .build(&event_loop)
    {
        Ok(window) => window,
        Err(err) => {
            desktop::startup_error(&format!("cannot open a window: {err}"));
            return 1;
        }
    };
    let monitors: Vec<_> = window.available_monitors().map(|m| {
        let p = m.position(); let s = m.size(); (p.x, p.y, s.width, s.height)
    }).collect();
    if prefs.visible_on(&monitors) {
        if let Some((x, y)) = prefs.position {
            window.set_outer_position(PhysicalPosition::new(x, y));
        }
    }
    window.set_maximized(prefs.maximized);

    // WebView2 keeps its profile and caches in a user data folder, and an unset one lands
    // next to the exe — a directory a PATH install does not own. Under the gateway's home it
    // follows SWISS_HOME like every other file this tool writes.
    let data = swiss_core::paths::data_dir().join("gui-webview");
    let mut web_context = WebContext::new(Some(data));
    let desktop_proxy = event_loop.create_proxy();
    let download_proxy = event_loop.create_proxy();
    let panel_started = std::rc::Rc::new(std::cell::Cell::new(false));
    let navigation_started = panel_started.clone();
    let webview_builder = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_bounds(full_bounds(window.inner_size().width, window.inner_size().height))
        .with_html(loading_html())
        .with_initialization_script("window.__SWISS_DESKTOP__ = true;")
        // The startup HTML is about:blank, and NavigateToString leaves it behind the panel in
        // WebView2 history. Refuse that entry after the panel starts, while keeping its own
        // same-origin history navigable by mouse and keyboard.
        .with_navigation_handler(move |uri| {
            if navigation_allowed(&uri, port, &navigation_started) { return true; }
            open_external(&uri);
            false
        })
        // Wry already permits downloads by default. Make the origin/destination policy
        // explicit and report actual completion instead of assuming an anchor click saved.
        .with_download_started_handler(move |uri, path| download_url(&uri, port)
            && path.is_absolute() && !path.components().any(|p| p == std::path::Component::ParentDir))
        .with_download_completed_handler(move |_, _, success| {
            let _ = download_proxy.send_event(GuiEvent::DownloadFinished(success));
        })
        .with_ipc_handler(move |request| {
            if let Some(message) = desktop::message(&request.uri().to_string(), port, request.body()) {
                let _ = desktop_proxy.send_event(GuiEvent::Desktop(message));
            }
        })
        // The window is the panel, not a browser. A link that asks for a new window — a
        // target="_blank" answer link, a data cell's URL, the MCP sign-in page, "Open UI" on
        // the process page — has to reach the operator's real browser, where their session and
        // their password manager live. wry's default with NO handler here is a silent denial on
        // both engines (WebView2 sets Handled, WKWebView returns a nil webview), so every such
        // click disappeared while the page had already told the user it opened (2026-11-01:
        // MCP authorization could not be completed from the desktop app at all).
        .with_new_window_req_handler(|uri, _features| {
            open_external(&uri);
            NewWindowResponse::Deny
        });
    #[cfg(windows)]
    let webview_builder = {
        use wry::WebViewBuilderExtWindows;
        webview_builder.with_default_context_menus(false)
    };
    // WKWebView ignores WebContext's directory. A distinct persistent data store isolates
    // cookies and localStorage even when two homes serve the same loopback hostname.
    #[cfg(target_os = "macos")]
    let webview_builder = {
        use wry::WebViewBuilderExtDarwin;
        webview_builder.with_data_store_identifier(webview_store_id(port, &swiss_core::paths::data_dir()))
    };
    let webview = match webview_builder.build(&window) {
        Ok(webview) => webview,
        Err(err) => {
            desktop::startup_error(&format!("cannot start the system webview: {err}"));
            return 1;
        }
    };
    if let Err(err) = webview.zoom(prefs.zoom) { eprintln!("cannot restore text size: {err}"); }
    window.set_visible(true);

    let open_item = MenuItem::with_id("open", "Open swiss", true, None);
    let quit_item = MenuItem::with_id("quit", "Exit swiss", true, None);
    let menu = Menu::new();
    if let Err(err) = menu
        .append(&open_item)
        .and_then(|_| menu.append(&PredefinedMenuItem::separator()))
        .and_then(|_| menu.append(&quit_item))
    {
        eprintln!("cannot build the tray menu: {err}");
        return 1;
    }
    let icon = match TrayIconPixels::from_rgba(rgba, width, height) {
        Ok(icon) => icon,
        Err(err) => {
            eprintln!("cannot draw the tray icon: {err}");
            return 1;
        }
    };
    // The tray handle is kept alive on purpose: dropping it removes the icon, so it lives as
    // long as the loop does — the underscore only silences the unused binding, not the keep.
    let tray_builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("swiss")
        .with_icon(icon);
    #[cfg(windows)]
    let _tray = match tray_builder.build() {
        Ok(tray) => tray,
        Err(err) => {
            eprintln!("cannot claim a tray slot: {err}");
            return 1;
        }
    };
    // AppKit needs a running event loop before installing a status-bar item.
    #[cfg(target_os = "macos")]
    let mut tray_builder = Some(tray_builder);
    #[cfg(target_os = "macos")]
    let mut _tray = None;

    // Tray and menu events arrive on global channels the tao loop never sees, so both are
    // forwarded into the loop as user events — the same wiring tauri uses under its own shell.
    let menu_proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.0.as_str() {
        "open" => {
            let _ = menu_proxy.send_event(GuiEvent::TrayOpen);
        }
        "quit" => {
            let _ = menu_proxy.send_event(GuiEvent::Quit);
        }
        _ => {}
    }));
    let tray_proxy = event_loop.create_proxy();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if matches!(event, TrayIconEvent::DoubleClick { .. }) {
            let _ = tray_proxy.send_event(GuiEvent::TrayOpen);
        }
    }));

    // The probe/spawn/mint sequence is async and can take a start's full thirty seconds on a
    // cold gateway; the loop must pump from the first frame, so the sequence runs beside it
    // and the window loads the link the moment it exists.
    let starter_proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let event = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            // No port here on purpose. `start_daemon` resolves it exactly the way this shell
            // already did — `SWISS_PORT`, then the home's config, then 19999 — but only a port
            // it was HANDED is persisted into gateway.config.json. Passing the resolved number
            // turned one launch under a temporary `SWISS_PORT` into a permanent port switch for
            // every later `swiss start` (2026-11-01).
            Ok(runtime) => match runtime.block_on(launch_sequence(StartOptions::default())) {
                Ok(url) => GuiEvent::PanelReady(url),
                Err(reason) => GuiEvent::Fatal(reason),
            },
            Err(err) => GuiEvent::Fatal(format!("cannot start the sign-in sequence: {err}")),
        };
        let _ = starter_proxy.send_event(event);
    });

    // The next `swiss gui` for THIS gateway signals the show event; this thread turns the
    // signal into the same user event the tray's open item sends. The handle crosses into
    // the thread as an integer because the windows crate's HANDLE is a raw pointer and so
    // not Send — the number is this process's own never-closed handle, only ever fed back
    // to WaitForSingleObject.
    //
    // `ready` separates "still coming up" from "was up and is gone". A nudge that arrives
    // before the panel ever loaded only means a concurrent launch lost the claim, so the
    // window stays; a nudge after it loaded, with nothing listening on the port, means the
    // gateway behind this window was quit or killed and the shell has to go with it.
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(windows)]
    if !show.is_invalid() {
        let waiter_proxy = event_loop.create_proxy();
        let waiter_ready = ready.clone();
        let show_event = show.0 as usize;
        std::thread::spawn(move || {
            use windows::Win32::Foundation::WAIT_OBJECT_0;
            use windows::Win32::System::Threading::{WaitForSingleObject, INFINITE};
            let show = windows::Win32::Foundation::HANDLE(show_event as *mut _);
            loop {
                // A failed wait — the handle gone — never turns healthy again by spinning.
                if unsafe { WaitForSingleObject(show, INFINITE) } != WAIT_OBJECT_0 {
                    return;
                }
                let gone = waiter_ready.load(std::sync::atomic::Ordering::Relaxed)
                    && !daemon::port_accepts(port);
                let _ = waiter_proxy.send_event(if gone { GuiEvent::Quit } else { GuiEvent::TrayOpen });
            }
        });
    }

    #[cfg(target_os = "macos")]
    {
        let signal_proxy = event_loop.create_proxy();
        let signal_ready = ready.clone();
        std::thread::spawn(move || signal.listen(move || {
            let gone = signal_ready.load(std::sync::atomic::Ordering::Relaxed)
                && !daemon::port_accepts(port);
            let _ = signal_proxy.send_event(if gone { GuiEvent::Quit } else { GuiEvent::TrayOpen });
        }));
    }

    let window_id = window.id();
    let mut fatal = false;
    let mut placement_dirty = true;
    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            #[cfg(target_os = "macos")]
            Event::NewEvents(tao::event::StartCause::Init) => {
                if let Some(builder) = tray_builder.take() {
                    match builder.build() {
                        Ok(tray) => _tray = Some(tray),
                        Err(error) => eprintln!("cannot claim a menu-bar slot: {error}"),
                    }
                }
            }
            Event::WindowEvent {
                window_id: id,
                event: WindowEvent::CloseRequested,
                ..
            } if id == window_id => {
                // Closing hides to the tray: the gateway is a service that outlives windows
                // (ADR-038), and quitting here would only make the next `swiss gui` mint
                // another sign-in. The tray's Exit is the way out — unless the gateway never
                // came up, when there is nothing left to keep.
                if fatal {
                    std::process::exit(1);
                }
                prefs.save(&prefs_path);
                window.set_visible(false);
            }
            Event::WindowEvent {
                window_id: id,
                event: WindowEvent::Resized(size),
                ..
            } if id == window_id => {
                let _ = webview.set_bounds(full_bounds(size.width, size.height));
                placement_dirty = true;
            }
            Event::WindowEvent { window_id: id, event: WindowEvent::Moved(_), .. } if id == window_id => {
                placement_dirty = true;
            }
            Event::MainEventsCleared if placement_dirty => {
                // Windows delivers WM_MOVE before the maximize flag settles. Sampling in
                // Moved stored the maximized frame's (-9,-9) as the normal restore position.
                // Read placement only after this message batch, keeping normal bounds intact.
                placement_dirty = false;
                let size = window.inner_size().to_logical::<u32>(window.scale_factor());
                let position = window.outer_position().ok().map(|p| (p.x, p.y));
                prefs.remember((size.width, size.height), position, window.is_maximized(), window.is_minimized());
            }
            Event::WindowEvent { window_id: id, event: WindowEvent::Focused(false), .. } if id == window_id => {
                prefs.save(&prefs_path);
            }
            Event::UserEvent(GuiEvent::PanelReady(url)) => {
                // From here on a nudge from a second launch means "this window has a gateway
                // behind it" only if something still answers on the port.
                ready.store(true, std::sync::atomic::Ordering::Relaxed);
                // The one-time token is spent by this load and the panel redirects itself to
                // a clean `/` (SPEC §host.session); nothing sensitive stays in any URL bar
                // because there is no URL bar.
                if let Err(err) = webview.load_url(&url) {
                    eprintln!("cannot load the panel: {err}");
                    fatal = true;
                    let reason = format!("cannot load the panel: {err}");
                    panel_started.set(false);
                    let _ = webview.load_html(&error_html(&reason));
                }
                window.set_focus();
            }
            Event::UserEvent(GuiEvent::Fatal(reason)) => {
                eprintln!("swiss gui: {reason}");
                panel_started.set(false);
                let _ = webview.load_html(&error_html(&reason));
                fatal = true;
                window.set_focus();
            }
            Event::UserEvent(GuiEvent::DownloadFinished(success)) => {
                let _ = webview.evaluate_script(&format!("window.dispatchEvent(new CustomEvent('swiss-download',{{detail:{{success:{success}}}}}));"));
            }
            Event::UserEvent(GuiEvent::TrayOpen) | Event::Reopen { .. } => {
                window.set_visible(true);
                window.set_minimized(false);
                window.set_focus();
            }
            Event::UserEvent(GuiEvent::Desktop(message)) => match message {
                desktop::Message::Theme(dark) => {
                    prefs.dark = Some(dark);
                    window.set_theme(Some(if dark { Theme::Dark } else { Theme::Light }));
                    prefs.save(&prefs_path);
                }
                desktop::Message::Title(title) => window.set_title(&title),
                desktop::Message::Quit => {
                    // Stop the tray and the loop the same way the tray's Exit does, and save the
                    // window before it goes: the gateway behind this window is already gone.
                    prefs.save(&prefs_path);
                    std::process::exit(0);
                }
                desktop::Message::Zoom(step) => {
                    let zoom = if step == 0 { 1.0 } else { ((prefs.zoom * 10.0).round() + f64::from(step)) / 10.0 }.clamp(0.8, 1.5);
                    match webview.zoom(zoom) {
                        Ok(()) => { prefs.zoom = zoom; prefs.save(&prefs_path); }
                        Err(err) => eprintln!("cannot change text size: {err}"),
                    }
                }
            },
            Event::UserEvent(GuiEvent::Quit) => { prefs.save(&prefs_path); std::process::exit(0); }
            Event::LoopDestroyed => prefs.save(&prefs_path),
            _ => {}
        }
    })
}

/// SPEC §host.gui: WKWebView's persistent profile belongs to one home and port.
#[cfg(any(target_os = "macos", test))]
fn webview_store_id(port: u16, home: &std::path::Path) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(home.as_os_str().as_encoded_bytes());
    hash.update([0]);
    hash.update(port.to_be_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&hash.finalize()[..16]);
    id
}

/// The webview covers the window's whole client area at the given pixel size.
#[cfg(any(windows, target_os = "macos"))]
fn full_bounds(width: u32, height: u32) -> Rect {
    Rect {
        position: Position::Physical(PhysicalPosition::new(0, 0)),
        size: WebViewSize::Physical(PhysicalSize::new(width, height)),
    }
}

/// The brand mark (assets/logo.svg) as 64x64 RGBA pixels: the red rounded tile, three white
/// tools fanned open from one rivet, and the rivet's red dot — the multitool. Drawn from the
/// SVG's own geometry (tile rx 14; blades 34/30/26 long at 0°/-32°/-64° around the pivot;
/// hub r 7; rivet r 2.4) so window, tray and panel carry one mark, smooth-edged via each
/// shape's signed distance and 2x2 supersampled so the fan stays legible at tray size.
#[cfg(any(windows, target_os = "macos"))]
fn brand_icon() -> (Vec<u8>, u32, u32) {
    const SIZE: u32 = 64;
    const RED: [f64; 3] = [218.0, 41.0, 28.0]; // #DA291C
    const PIVOT: (f64, f64) = (18.0, 46.0);
    // Coverage of a shape from the signed distance to it: solid inside, gone outside, one
    // pixel of falloff across the edge it crosses.
    let coverage = |distance: f64| (0.5 - distance).clamp(0.0, 1.0);
    // The tile: distance to the core rectangle [radius, SIZE-radius]² minus the corner
    // radius — the SVG's full-canvas rect with rx 14.
    let tile = |x: f64, y: f64| {
        const RADIUS: f64 = 14.0;
        let qx = x.clamp(RADIUS, f64::from(SIZE) - RADIUS);
        let qy = y.clamp(RADIUS, f64::from(SIZE) - RADIUS);
        ((x - qx).powi(2) + (y - qy).powi(2)).sqrt() - RADIUS
    };
    // One fanned tool: a capsule of half-thickness 4 whose skeleton runs from the pivot
    // along `angle_deg` for `length` — exactly the SVG's 8-tall rect with rx 4, rotated
    // about the pivot.
    let blade = |x: f64, y: f64, angle_deg: f64, length: f64| {
        const HALF: f64 = 4.0;
        let (sin, cos) = angle_deg.to_radians().sin_cos();
        let ax = PIVOT.0 + HALF * cos;
        let ay = PIVOT.1 + HALF * sin;
        let bx = PIVOT.0 + (length - HALF) * cos;
        let by = PIVOT.1 + (length - HALF) * sin;
        let vx = x - ax;
        let vy = y - ay;
        let span = (bx - ax) * (bx - ax) + (by - ay) * (by - ay);
        let t = ((vx * (bx - ax) + vy * (by - ay)) / span).clamp(0.0, 1.0);
        let qx = ax + t * (bx - ax);
        let qy = ay + t * (by - ay);
        ((x - qx).powi(2) + (y - qy).powi(2)).sqrt() - HALF
    };
    let circle = |x: f64, y: f64, r: f64| {
        ((x - PIVOT.0).powi(2) + (y - PIVOT.1).powi(2)).sqrt() - r
    };
    // White over red, then the red rivet over the white hub. Every shape sits inside the
    // tile, so the tile's coverage is the pixel's alpha and nothing extends past it.
    let over = |top: [f64; 3], top_cov: f64, bottom: [f64; 3]| {
        [
            top[0] * top_cov + bottom[0] * (1.0 - top_cov),
            top[1] * top_cov + bottom[1] * (1.0 - top_cov),
            top[2] * top_cov + bottom[2] * (1.0 - top_cov),
        ]
    };
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (mut red, mut green, mut blue, mut alpha) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let px = f64::from(x) + dx;
                let py = f64::from(y) + dy;
                let white = coverage(blade(px, py, 0.0, 34.0))
                    .max(coverage(blade(px, py, -32.0, 30.0)))
                    .max(coverage(blade(px, py, -64.0, 26.0)))
                    .max(coverage(circle(px, py, 7.0)));
                let rivet = coverage(circle(px, py, 2.4));
                let color = over(RED, rivet, over([255.0; 3], white, RED));
                red += color[0];
                green += color[1];
                blue += color[2];
                alpha += coverage(tile(px, py));
            }
            rgba.extend_from_slice(&[
                (red / 4.0).round() as u8,
                (green / 4.0).round() as u8,
                (blue / 4.0).round() as u8,
                (alpha / 4.0 * 255.0).round() as u8,
            ]);
        }
    }
    (rgba, SIZE, SIZE)
}

/// What the window shows while the sign-in link is being minted. Self-contained and bilingual
/// like the lock page (SPEC §host.session): it must speak before any panel code loads.
#[cfg(any(windows, target_os = "macos"))]
fn loading_html() -> String {
    page_html(
        "Opening the swiss panel…",
        "正在打开 swiss 面板……",
        "",
    )
}

/// What the window shows when the gateway never came up: the reason, and where the whole log
/// is. The reason carries a log tail, so it goes in as text, never as markup.
#[cfg(any(windows, target_os = "macos"))]
fn error_html(reason: &str) -> String {
    page_html(
        "The panel cannot open.",
        "面板无法打开。",
        &format!(
            "<pre>{}</pre><p>Run <code>swiss logs</code> for the whole log.</p>",
            escape_html(reason)
        ),
    )
}

/// The one page template the shell itself ever paints, styled after the lock page so the
/// window looks like the product before the product loads. `detail` is trusted markup;
/// callers escape what they put in it.
#[cfg(any(windows, target_os = "macos"))]
fn page_html(en: &str, zh: &str, detail: &str) -> String {
    format!(
        concat!(
            "<!doctype html><html><head><meta charset=\"utf-8\">",
            "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
            "<title>swiss</title><style>",
            "body{{font:15px/1.6 system-ui,sans-serif;margin:0;display:grid;place-items:center;",
            "min-height:100vh;background:#f7f7f8;color:#1f2328}}",
            "main{{max-width:34rem;padding:2rem}}p{{margin:.4rem 0}}",
            "code{{background:#eaeef2;padding:.15rem .4rem;border-radius:6px;font-size:14px}}",
            "pre{{white-space:pre-wrap;background:#eaeef2;padding:.8rem;border-radius:8px;",
            "font-size:13px;max-height:50vh;overflow:auto}}",
            "@media (prefers-color-scheme:dark){{body{{background:#101114;color:#e6e6e6}}",
            "code,pre{{background:#26282e}}}}</style></head><body><main>",
            "<p><strong>{en}</strong></p>",
            "<p lang=\"zh\"><strong>{zh}</strong></p>",
            "{detail}",
            "</main></body></html>"
        ),
        en = en,
        zh = zh,
        detail = detail,
    )
}

/// The three characters a log tail may carry that HTML would otherwise take as markup.
#[cfg(any(windows, target_os = "macos"))]
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    // launch_sequence is the CLI-shaped half of `swiss gui`, so it is tested the way the CLI
    // is: against a fake gateway that really serves the endpoints the sequence talks to, and
    // against an entry that cannot spawn. The window half has no test double — a message loop
    // cannot be driven from a test — and is proven live instead (SPEC §testing.live).
    use super::*;
    use crate::pidfile::log_file_path;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn a_detached_launch_reenters_as_one_foreground_window() {
        assert_eq!(foreground_args(19999), ["gui", "--foreground", "--port", "19999"]);
    }

    /// A window request is a hand-off to the operator's browser, so it may only ever carry a
    /// web link. The panel renders model output and database cells, which is exactly where a
    /// `file:` or `javascript:` payload would come from.
    #[test]
    fn a_new_window_request_only_reaches_the_browser_for_web_links() {
        assert_eq!(external_url("https://example.test/auth?x=1").as_deref(), Some("https://example.test/auth?x=1"));
        assert!(external_url("http://127.0.0.1:19999/api/session/ticket?t=abc").is_some());
        for refused in ["file:///C:/Windows/win.ini", "javascript:alert(1)", "data:text/html,<b>x</b>", "mailto:someone@example.test", "not a url", ""] {
            assert_eq!(external_url(refused), None, "{refused}");
        }
    }

    #[test]
    fn navigation_and_downloads_are_scoped_to_the_selected_gateway() {
        assert!(panel_url("http://127.0.0.1:19998/?token=one", 19998));
        assert!(download_url("blob:http://127.0.0.1:19998/a-result", 19998));
        for uri in ["http://127.0.0.1:19999/", "https://example.test/", "http://127.0.0.1.evil.test:19998/", "http://user@127.0.0.1:19998/", "file:///tmp/result", "data:text/html,test"] {
            assert!(!panel_url(uri, 19998), "{uri}");
            assert!(!download_url(uri, 19998), "{uri}");
        }
        assert!(!download_url("blob:http://127.0.0.1:19999/result", 19998));
    }

    #[test]
    fn navigation_cannot_return_to_the_startup_page() {
        let panel_started = std::cell::Cell::new(false);
        assert!(navigation_allowed("about:blank", 19998, &panel_started));
        assert!(navigation_allowed("http://127.0.0.1:19998/?token=once", 19998, &panel_started));
        assert!(navigation_allowed("http://127.0.0.1:19998/#data", 19998, &panel_started));
        assert!(navigation_allowed("http://127.0.0.1:19998/#mcp", 19998, &panel_started));
        assert!(!navigation_allowed("about:blank", 19998, &panel_started));
        assert!(!navigation_allowed("http://127.0.0.1:19999/", 19998, &panel_started));
    }

    #[test]
    fn the_mac_webview_store_is_stable_and_isolates_homes_and_ports() {
        let home = std::path::Path::new("/Users/example/.swiss");
        let id = webview_store_id(19999, home);
        assert_eq!(id, webview_store_id(19999, home));
        assert_ne!(id, webview_store_id(19998, home));
        assert_ne!(id, webview_store_id(19999, std::path::Path::new("/Users/example/.swiss-test-home")));
    }

    /// The scratch home every test in this module shares, mirroring daemon's: a test home, the
    /// deterministic master key, and the data-dir lock so state writes never race.
    async fn gui_state() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = swiss_core::paths::DATA_DIR_LOCK.lock().await;
        swiss_core::paths::test_home();
        swiss_core::secure::key::use_test_master_key();
        guard
    }

    /// A loopback stand-in for a running gateway: /health the start path polls and the ticket
    /// route the sign-in mints from, serving a fixed one-time link.
    struct FakeGateway {
        port: u16,
        serving: tokio::task::JoinHandle<()>,
    }

    impl FakeGateway {
        async fn start() -> Self {
            use axum::routing::{get, post};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            let port = listener.local_addr().expect("a bound address").port();
            let app = axum::Router::new()
                .route("/health", get(|| async { axum::Json(json!({ "ok": true })) }))
                .route(
                    "/api/session/ticket",
                    // The link carries the port it is actually served on, the way the real
                    // route answers with ctx.port (SPEC §host.session).
                    post(move || async move {
                        axum::Json(json!({
                            "token": "one-time",
                            "url": format!("http://127.0.0.1:{port}/?token=one-time"),
                        }))
                    }),
                );
            let serving = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            Self { port, serving }
        }

        fn stop(self) {
            self.serving.abort();
        }
    }

    /// A port nothing is listening on: bound, read back, released.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("the loopback has a free port")
            .local_addr()
            .expect("a bound listener has an address")
            .port()
    }

    #[tokio::test]
    async fn a_running_gateway_yields_its_sign_in_link() {
        // No pid file planted: a serve started outside `swiss start` leaves none, and the
        // starter must recognize the healthy port and mint against it all the same
        // (2026-10-30: it spawned a doomed second serve instead).
        let _lock = gui_state().await;
        let gw = FakeGateway::start().await;
        let url = launch_sequence(StartOptions {
            port: Some(gw.port),
            ..Default::default()
        })
        .await
        .expect("an up gateway yields a link");
        assert_eq!(url, format!("http://127.0.0.1:{}/?token=one-time", gw.port));
        gw.stop();
    }

    #[tokio::test]
    async fn a_gateway_that_never_comes_up_reports_the_log_tail() {
        let _lock = gui_state().await;
        let port = free_port();
        let reason = launch_sequence(StartOptions {
            port: Some(port),
            // Unspawnable, so the failure is instant and never touches a real gateway.
            entry: Some(PathBuf::from("swiss-no-such-binary")),
            timeout_ms: Some(200),
            ..Default::default()
        })
        .await
        .expect_err("a start that never spawned is a failure");
        assert!(reason.contains("spawn failed"), "{reason}");
        let _ = std::fs::remove_file(log_file_path(port));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn the_brand_icon_draws_the_mark_not_a_plain_tile() {
        let (rgba, width, height) = brand_icon();
        assert_eq!((width, height), (64, 64));
        let at = |x: usize, y: usize| {
            let i = (y * 64 + x) * 4;
            (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
        };
        // Points picked at least 1.5px off every edge, so supersampling is settled: open
        // tile, the longest tool, the hub's white ring, the rivet's dot, and the corner the
        // tile's radius leaves clear.
        assert_eq!(at(10, 20), (0xDA, 0x29, 0x1C, 0xFF)); // open tile
        assert_eq!(at(26, 46), (0xFF, 0xFF, 0xFF, 0xFF)); // the longest tool
        assert_eq!(at(18, 40), (0xFF, 0xFF, 0xFF, 0xFF)); // the hub above the tools
        assert_eq!(at(18, 46), (0xDA, 0x29, 0x1C, 0xFF)); // the rivet's red dot
        let (_, _, _, corner) = at(0, 0);
        assert_eq!(corner, 0x00); // outside the tile's radius
    }

    /// The window claim is per GATEWAY, not per desktop: a second home or a second port is
    /// a second gateway and must get its own window — the config-switch isolation the
    /// desktop-wide mutex of the first cut broke (2026-10-30).
    #[cfg(windows)]
    #[test]
    fn one_window_per_gateway_not_per_desktop() {
        let home = std::path::Path::new(r"C:\Users\x\.swiss");
        let a = instance_id(19999, home);
        assert_eq!(a, instance_id(19999, home));
        assert_ne!(
            a,
            instance_id(19999, std::path::Path::new(r"C:\Users\x\.swiss-test-home")),
            "a second home is a second gateway and gets its own window"
        );
        assert_ne!(
            a,
            instance_id(19998, home),
            "a second port is a second gateway too"
        );
        // Windows paths differ by case alone: still one gateway, one window.
        assert_eq!(a, instance_id(19999, std::path::Path::new(r"c:\users\x\.swiss")));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn an_error_page_carries_the_reason_as_text_never_as_markup() {
        let page = error_html("spawn failed: <script>alert(1)</script>");
        assert!(page.contains("&lt;script&gt;"), "{page}");
        assert!(!page.contains("<script>"), "{page}");
    }
}
