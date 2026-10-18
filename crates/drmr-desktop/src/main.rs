#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use drmr_desktop::{ConsoleOrigin, bundled_cli, permits_startup_navigation};
use drmr_server::{RunningServer, ServerConfig};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

const IDLE: u8 = 0;
const STARTING: u8 = 1;
const RUNNING: u8 = 2;
const CLOSING: u8 = 3;
const UNCONFIRMED: u8 = 4;

#[derive(Default)]
struct Roots {
    catalog: Option<PathBuf>,
    output_parent: Option<PathBuf>,
}

#[derive(Default)]
struct Desktop {
    roots: Mutex<Roots>,
    origin: Mutex<Option<ConsoleOrigin>>,
    server: Mutex<Option<RunningServer>>,
    phase: AtomicU8,
    picking: AtomicBool,
    exit_allowed: AtomicBool,
}

fn error(app: &AppHandle, message: &'static str) {
    app.dialog()
        .message(message)
        .title("drmr")
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

fn title(app: &AppHandle, value: &'static str) {
    if let Some(window) = app.get_webview_window("console") {
        let _ = window.set_title(value);
    }
}

fn pick(app: &AppHandle, output: bool) {
    let state = app.state::<Arc<Desktop>>();
    if state.phase.load(Ordering::Acquire) != IDLE {
        error(
            app,
            "Folders cannot change while the server is running. Quit and reopen drmr to use different folders.",
        );
        return;
    }
    if state.picking.swap(true, Ordering::AcqRel) {
        return;
    }
    let handle = app.clone();
    let caption = if output {
        "Choose the parent folder for private drmr output"
    } else {
        "Choose the catalog folder"
    };
    let dialog = app.dialog().file().set_title(caption);
    let dialog = if let Some(window) = app.get_webview_window("console") {
        dialog.set_parent(&window)
    } else {
        dialog
    };
    dialog.pick_folder(move |selection| {
        let state = handle.state::<Arc<Desktop>>();
        if let Some(selection) = selection {
            match (selection.into_path(), state.roots.lock()) {
                (Ok(path), Ok(mut roots)) if state.phase.load(Ordering::Acquire) == IDLE => {
                    if output {
                        roots.output_parent = Some(path);
                    } else {
                        roots.catalog = Some(path);
                    }
                    title(
                        &handle,
                        if roots.catalog.is_some() && roots.output_parent.is_some() {
                            "drmr — folders selected"
                        } else {
                            "drmr — choose both folders"
                        },
                    );
                }
                _ => error(&handle, "The selected folder could not be used."),
            }
        }
        state.picking.store(false, Ordering::Release);
    });
}

fn checked_cli() -> std::io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let cli =
        bundled_cli(&executable).ok_or_else(|| std::io::Error::other("bundled-cli-unavailable"))?;
    let mut file = std::fs::File::open(&cli)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    if format!("{:x}", hash.finalize()) != env!("DRMR_CLI_SHA256") {
        return Err(std::io::Error::other("bundled-cli-mismatch"));
    }
    Ok(cli)
}

fn finish(app: &AppHandle, state: &Desktop) {
    let confirmed = match state.server.lock() {
        Ok(mut server) => match server.as_mut() {
            Some(server) => server.shutdown().is_ok(),
            None => true,
        },
        Err(_) => false,
    };
    if confirmed {
        state.exit_allowed.store(true, Ordering::Release);
        app.exit(0);
    } else {
        // Keep the server, manager and process handles owned after an unconfirmed cleanup.
        state.phase.store(UNCONFIRMED, Ordering::Release);
        title(app, "drmr — cleanup unconfirmed");
        error(
            app,
            "Cleanup could not be confirmed. drmr is still open and retains ownership of its running work. Check the job log, resolve the cause and select Quit drmr again to retry cleanup.",
        );
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CloseAction {
    Exit,
    WaitForStartup,
    Shutdown { previous: u8 },
}

fn claim_close(phase: &AtomicU8) -> Option<CloseAction> {
    loop {
        let current = phase.load(Ordering::Acquire);
        let action = match current {
            IDLE => CloseAction::Exit,
            STARTING => CloseAction::WaitForStartup,
            RUNNING | UNCONFIRMED => CloseAction::Shutdown { previous: current },
            _ => return None,
        };
        if phase
            .compare_exchange(current, CLOSING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Some(action);
        }
    }
}

fn request_close(app: &AppHandle) {
    let state = app.state::<Arc<Desktop>>().inner().clone();
    match claim_close(&state.phase) {
        Some(CloseAction::Exit) => {
            state.exit_allowed.store(true, Ordering::Release);
            app.exit(0);
        }
        Some(CloseAction::WaitForStartup) => {
            // The startup worker owns completion and drains any server it creates.
            title(app, "drmr — closing");
        }
        Some(CloseAction::Shutdown { previous }) => {
            title(app, "drmr — closing");
            let handle = app.clone();
            let worker_state = state.clone();
            if std::thread::Builder::new()
                .name("drmr-desktop-shutdown".into())
                .spawn(move || finish(&handle, &worker_state))
                .is_err()
            {
                state.phase.store(previous, Ordering::Release);
                error(
                    app,
                    "The shutdown worker could not start. drmr remains open. Select Quit drmr again to retry.",
                );
            }
        }
        None => {}
    }
}

fn open_console(app: &AppHandle) {
    let state = app.state::<Arc<Desktop>>().inner().clone();
    if state.picking.load(Ordering::Acquire) {
        return;
    }
    if state.phase.load(Ordering::Acquire) == RUNNING {
        let url = state
            .origin
            .lock()
            .ok()
            .and_then(|origin| origin.as_ref().map(|origin| origin.url().clone()));
        if let (Some(window), Some(url)) = (app.get_webview_window("console"), url) {
            let _ = window.navigate(url);
        }
        return;
    }
    if state
        .phase
        .compare_exchange(IDLE, STARTING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let roots = state
        .roots
        .lock()
        .ok()
        .and_then(|roots| Some((roots.catalog.clone()?, roots.output_parent.clone()?)));
    let Some((catalog, output_parent)) = roots else {
        state.phase.store(IDLE, Ordering::Release);
        error(
            app,
            "Choose both a catalog folder and an output location from the File menu.",
        );
        return;
    };
    title(app, "drmr — starting");
    let handle = app.clone();
    let worker_state = state.clone();
    if std::thread::Builder::new().name("drmr-desktop-startup".into()).spawn(move || {
        let result = (|| {
            let cli = checked_cli().map_err(|_| ())?;
            let mut config = ServerConfig::new(catalog, output_parent.join("drmr-output"));
            "127.0.0.1".clone_into(&mut config.bind_host);
            config.bind_port = 0;
            config.cli_argv = vec![cli.into_os_string()];
            config.batch_available = true;
            drmr_server::spawn(config).map_err(|_| ())
        })();
        match result {
            Ok(server) => {
                let Some(origin) = ConsoleOrigin::new(server.addr) else {
                    // spawn is configured for an ephemeral IPv4 loopback listener.
                    *worker_state.server.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(server);
                    worker_state.phase.store(CLOSING, Ordering::Release);
                    finish(&handle, &worker_state);
                    return;
                };
                *worker_state.server.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(server);
                if let Ok(mut allowed) = worker_state.origin.lock() { *allowed = Some(origin.clone()); }
                if worker_state.phase.compare_exchange(STARTING, RUNNING, Ordering::AcqRel, Ordering::Acquire).is_err() {
                    finish(&handle, &worker_state);
                    return;
                }
                if handle.get_webview_window("console").is_none_or(|window| window.navigate(origin.url().clone()).is_err()) {
                    error(&handle, "The console could not be opened. drmr will stop its local server.");
                    request_close(&handle);
                } else {
                    title(&handle, "drmr");
                }
            }
            Err(()) => {
                if worker_state.phase.compare_exchange(STARTING, IDLE, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    title(&handle, "drmr — startup failed");
                    error(&handle, "The local server could not start. Check that the bundled CLI is intact and that the chosen output location can contain private drmr files.");
                } else {
                    finish(&handle, &worker_state);
                }
            }
        }
    }).is_err() {
        state.phase.store(IDLE, Ordering::Release);
        error(app, "The startup worker could not start.");
    }
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let catalog = MenuItem::with_id(app, "catalog", "Choose catalog folder…", true, None::<&str>)?;
    let output = MenuItem::with_id(app, "output", "Choose output location…", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open console", true, Some("CmdOrCtrl+O"))?;
    let quit = MenuItem::with_id(app, "quit", "Quit drmr", true, Some("CmdOrCtrl+Q"))?;
    let application = Submenu::with_items(
        app,
        "drmr",
        true,
        &[&PredefinedMenuItem::about(app, None, None)?, &quit],
    )?;
    let file = Submenu::with_items(app, "File", true, &[&catalog, &output, &open])?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&application, &file, &edit])
}

fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--version" {
        println!("drmr-desktop {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    if arguments.len() == 1 && arguments[0] == "--help" {
        println!(
            "drmr-desktop [--help | --version]\n\nOpen the native drmr console. Use the File menu to choose catalog and output folders."
        );
        return std::process::ExitCode::SUCCESS;
    }
    if !arguments.is_empty() {
        eprintln!("drmr-desktop: unsupported-arguments; use --help");
        return std::process::ExitCode::from(2);
    }
    let state = Arc::new(Desktop::default());
    let navigation = state.clone();
    let built = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .menu(menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "catalog" => pick(app, false),
            "output" => pick(app, true),
            "open" => open_console(app),
            "quit" => request_close(app),
            _ => {}
        })
        .setup(move |app| {
            let links = app.handle().clone();
            WebviewWindowBuilder::new(app, "console", WebviewUrl::App("index.html".into()))
                .title("drmr — choose folders")
                .inner_size(1100.0, 760.0)
                .min_inner_size(320.0, 480.0)
                .on_navigation(move |url| {
                    navigation.origin.lock().is_ok_and(|origin| {
                        origin.as_ref().map_or_else(
                            || permits_startup_navigation(url),
                            |origin| origin.permits_navigation(url),
                        )
                    })
                })
                .on_new_window(move |url, _| {
                    let allowed = links
                        .state::<Arc<Desktop>>()
                        .origin
                        .lock()
                        .is_ok_and(|origin| {
                            origin
                                .as_ref()
                                .is_some_and(|origin| origin.permits_navigation(&url))
                        });
                    if allowed && let Some(window) = links.get_webview_window("console") {
                        let _ = window.navigate(url);
                    }
                    NewWindowResponse::Deny
                })
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!());
    let Ok(app) = built else {
        eprintln!("drmr: desktop-startup-failed");
        return std::process::ExitCode::FAILURE;
    };
    app.run(|handle, event| match event {
        RunEvent::ExitRequested { api, .. }
            if !handle
                .state::<Arc<Desktop>>()
                .exit_allowed
                .load(Ordering::Acquire) =>
        {
            api.prevent_exit();
            request_close(handle);
        }
        RunEvent::WindowEvent {
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } => {
            api.prevent_close();
            request_close(handle);
        }
        _ => {}
    });
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_claims_keep_idle_startup_and_running_behavior() {
        for (initial, expected) in [
            (IDLE, CloseAction::Exit),
            (STARTING, CloseAction::WaitForStartup),
            (RUNNING, CloseAction::Shutdown { previous: RUNNING }),
        ] {
            let phase = AtomicU8::new(initial);
            assert_eq!(claim_close(&phase), Some(expected));
            assert_eq!(phase.load(Ordering::Acquire), CLOSING);
            assert_eq!(claim_close(&phase), None);
        }
    }

    #[test]
    fn an_unconfirmed_cleanup_can_be_deliberately_retried_after_each_failure() {
        let phase = AtomicU8::new(UNCONFIRMED);
        for _ in 0..2 {
            assert_eq!(
                claim_close(&phase),
                Some(CloseAction::Shutdown {
                    previous: UNCONFIRMED
                })
            );
            assert_eq!(phase.load(Ordering::Acquire), CLOSING);
            assert_eq!(claim_close(&phase), None);
            phase.store(UNCONFIRMED, Ordering::Release);
        }
    }

    #[test]
    fn simultaneous_close_requests_claim_only_one_cleanup_worker() {
        let phase = Arc::new(AtomicU8::new(UNCONFIRMED));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let phase = phase.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    claim_close(&phase)
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .filter_map(|worker| worker.join().unwrap())
                .count(),
            1
        );
        assert_eq!(phase.load(Ordering::Acquire), CLOSING);
    }
}
