#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use drmr_desktop::plan_export::{Notice, PlanExports, Selection};
use drmr_desktop::{ConsoleOrigin, bundled_cli, permits_startup_navigation};
use drmr_server::{RunningServer, ServerConfig};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::webview::{DownloadEvent, NewWindowResponse};
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
    private_config: Option<PathBuf>,
}

#[derive(Clone, Copy)]
enum FolderRole {
    Input,
    Output,
    PrivateConfig,
}

#[derive(Default)]
struct Desktop {
    roots: Mutex<Roots>,
    origin: Mutex<Option<ConsoleOrigin>>,
    server: Mutex<Option<RunningServer>>,
    exports: Mutex<Option<Arc<PlanExports>>>,
    diagnostics: Mutex<Option<Arc<drmr_core::tool_diagnostics::DiagnosticSession>>>,
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

fn diagnose_tools(app: &AppHandle) {
    let state = app.state::<Arc<Desktop>>().inner().clone();
    if state
        .phase
        .compare_exchange(IDLE, STARTING, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        error(
            app,
            "Check tools before opening the console. Quit and reopen drmr to check a different launch environment.",
        );
        return;
    }
    let session = Arc::new(drmr_core::tool_diagnostics::DiagnosticSession::new(
        drmr_core::tool_diagnostics::Profile::ALL,
        true,
    ));
    if let Ok(mut current) = state.diagnostics.lock() {
        *current = Some(Arc::clone(&session));
    } else {
        state.phase.store(UNCONFIRMED, Ordering::Release);
        error(
            app,
            "The diagnostic owner could not be registered. Quit drmr to retry cleanup.",
        );
        return;
    }
    title(app, "drmr — checking tools");
    let handle = app.clone();
    let worker_state = state.clone();
    if std::thread::Builder::new()
        .name("drmr-tool-diagnostics".into())
        .spawn(move || {
            if worker_state.phase.load(Ordering::Acquire) != STARTING { session.request_stop(); }
            let report = session.run();
            match complete_diagnostics(&worker_state.phase, session.cleanup_confirmed()) {
                DiagnosticCompletion::Show => {
                let Ok(report) = report else {
                    error(&handle, "The diagnostic operation could not complete. Try Check external tools again.");
                    return;
                };
                title(&handle, "drmr — tool check complete");
                let kind = if report.ready {
                    MessageDialogKind::Info
                } else {
                    MessageDialogKind::Warning
                };
                handle
                    .dialog()
                    .message(report.summary())
                    .title("External media tools")
                    .kind(kind)
                    .show(|_| {});
                }
                DiagnosticCompletion::Close => finish(&handle, &worker_state),
                DiagnosticCompletion::Unconfirmed => {
                    title(&handle, "drmr — cleanup unconfirmed");
                    error(&handle, "A diagnostic tool's cleanup could not be confirmed. drmr retains its process owner. Select Quit drmr to retry cleanup; opening the console is blocked until cleanup is confirmed.");
                }
            }
        })
        .is_err()
    {
        if state
            .phase
            .compare_exchange(STARTING, IDLE, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            error(
                app,
                "The diagnostic worker could not start. Try Check external tools again.",
            );
        } else {
            finish(app, &state);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum DiagnosticCompletion {
    Show,
    Close,
    Unconfirmed,
}

fn complete_diagnostics(phase: &AtomicU8, cleanup_confirmed: bool) -> DiagnosticCompletion {
    if !cleanup_confirmed {
        phase.store(UNCONFIRMED, Ordering::Release);
        DiagnosticCompletion::Unconfirmed
    } else if phase
        .compare_exchange(STARTING, IDLE, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        DiagnosticCompletion::Show
    } else {
        DiagnosticCompletion::Close
    }
}

fn diagnostic_cleanup_confirmed(state: &Desktop) -> bool {
    state.diagnostics.lock().is_ok_and(|session| {
        session
            .as_ref()
            .is_none_or(|session| session.cleanup_confirmed())
    })
}

fn retry_diagnostic_cleanup(state: &Desktop) {
    let session = state
        .diagnostics
        .lock()
        .ok()
        .and_then(|session| session.clone());
    if let Some(session) = session {
        let _ = session.retry_cleanup();
    }
}

fn pick(app: &AppHandle, role: FolderRole) {
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
    let caption = match role {
        FolderRole::Input => "Choose the input folder",
        FolderRole::Output => "Choose the parent folder for private drmr output",
        FolderRole::PrivateConfig => "Choose a separate folder for private keys and fetch settings",
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
                    match role {
                        FolderRole::Input => roots.catalog = Some(path),
                        FolderRole::Output => roots.output_parent = Some(path),
                        FolderRole::PrivateConfig => roots.private_config = Some(path),
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

fn clear_private_config(app: &AppHandle) {
    let state = app.state::<Arc<Desktop>>();
    if state.phase.load(Ordering::Acquire) != IDLE || state.picking.load(Ordering::Acquire) {
        error(
            app,
            "Folders cannot change while the server or a folder dialog is active.",
        );
        return;
    }
    if let Ok(mut roots) = state.roots.lock() {
        roots.private_config = None;
    }
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
    let exports_confirmed = state
        .exports
        .lock()
        .ok()
        .map(|exports| exports.clone())
        .is_some_and(|exports| exports.is_none_or(|exports| exports.shutdown()));
    let confirmed = match state.server.lock() {
        Ok(mut server) => match server.as_mut() {
            Some(server) => server.shutdown().is_ok(),
            None => true,
        },
        Err(_) => false,
    };
    if confirmed && exports_confirmed && diagnostic_cleanup_confirmed(state) {
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
    // Fence chooser callbacks and file writes before changing the host phase.
    let choosing = state
        .exports
        .lock()
        .ok()
        .and_then(|exports| exports.clone())
        .is_some_and(|exports| exports.request_stop());
    match claim_close(&state.phase) {
        Some(CloseAction::Exit) => {
            state.exit_allowed.store(true, Ordering::Release);
            app.exit(0);
        }
        Some(CloseAction::WaitForStartup) => {
            // The startup/diagnostic worker owns completion before final cleanup.
            if let Ok(session) = state.diagnostics.lock()
                && let Some(session) = session.as_ref()
            {
                session.request_stop();
            }
            title(app, "drmr — closing");
        }
        Some(CloseAction::Shutdown { previous }) => {
            title(
                app,
                if choosing {
                    "drmr — cancel the save dialog to finish closing"
                } else {
                    "drmr — closing"
                },
            );
            let handle = app.clone();
            let worker_state = state.clone();
            if std::thread::Builder::new()
                .name("drmr-desktop-shutdown".into())
                .spawn(move || {
                    if previous == UNCONFIRMED {
                        retry_diagnostic_cleanup(&worker_state);
                    }
                    finish(&handle, &worker_state);
                })
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

fn export_notice(app: &AppHandle, notice: Notice) {
    if app.state::<Arc<Desktop>>().phase.load(Ordering::Acquire) != RUNNING {
        return;
    }
    match notice {
        Notice::Saved => title(app, "drmr — private plan copy saved"),
        Notice::Failed => error(
            app,
            "The plan copy could not be saved. Choose a new file in a private folder and try again. Existing files are never overwritten.",
        ),
        Notice::ChoosePrivateFile => error(
            app,
            "Choose a new JSON file in a private folder outside the input folder and drmr-output. Existing files are never overwritten.",
        ),
        Notice::CleanupUnconfirmed => error(
            app,
            "The partial plan copy could not be removed safely. drmr retains its file owner. Inspect the selected private folder, including hidden .drmr-plan-cleanup- folders, and preserve unfamiliar files. Restore the original private folder and select Quit drmr to retry cleanup.",
        ),
    }
}

fn download_plan(app: &AppHandle, url: &url::Url) {
    let state = app.state::<Arc<Desktop>>();
    if state.phase.load(Ordering::Acquire) != RUNNING {
        return;
    }
    let exports = state
        .exports
        .lock()
        .ok()
        .and_then(|exports| exports.clone());
    let Some(exports) = exports else {
        return;
    };
    if !exports.request(url) {
        title(app, "drmr — download unavailable");
    }
}

fn native_download(app: &AppHandle, event: DownloadEvent<'_>) -> bool {
    if let DownloadEvent::Requested { url, .. } = event {
        download_plan(app, &url);
    }
    // Do not hand a destination to the platform download delegate.
    false
}

fn choose_plan_file(app: &AppHandle, selection: Selection) {
    let state = app.state::<Arc<Desktop>>();
    let exports = state
        .exports
        .lock()
        .ok()
        .and_then(|exports| exports.clone());
    let Some(exports) = exports else {
        return;
    };
    if state.phase.load(Ordering::Acquire) != RUNNING || !exports.chooser_is_current(&selection) {
        exports.selected(selection, None);
        return;
    }
    let mut dialog = app
        .dialog()
        .file()
        .set_title("Save a private plan copy outside the media folders")
        .set_directory(selection.directory())
        .set_file_name("drmr-plan.json")
        .add_filter("JSON", &["json"]);
    if let Some(window) = app.get_webview_window("console") {
        dialog = dialog.set_parent(&window);
    }
    dialog.save_file(move |path| {
        exports.selected(selection, path.and_then(|path| path.into_path().ok()));
    });
}

fn failed_plan_start(app: &AppHandle, state: &Desktop) {
    let confirmed = state.server.lock().is_ok_and(|mut server| {
        let confirmed = server
            .as_mut()
            .is_none_or(|server| server.shutdown().is_ok());
        if confirmed {
            *server = None;
        }
        confirmed
    });
    if !confirmed {
        state.phase.store(UNCONFIRMED, Ordering::Release);
        title(app, "drmr — cleanup unconfirmed");
        error(
            app,
            "The private plan folder could not be prepared, and server cleanup could not be confirmed. Select Quit drmr to retry cleanup.",
        );
    } else if state
        .phase
        .compare_exchange(STARTING, IDLE, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        title(app, "drmr — choose a separate private folder");
        error(
            app,
            "The private plan folder could not be prepared outside the media folders. Choose a separate private configuration folder or another output location, then open the console again. Existing folder permissions have not been changed.",
        );
    } else {
        finish(app, state);
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
    let roots = state.roots.lock().ok().and_then(|roots| {
        Some((
            roots.catalog.clone()?,
            roots.output_parent.clone()?,
            roots.private_config.clone(),
        ))
    });
    let Some((catalog, output_parent, private_config)) = roots else {
        state.phase.store(IDLE, Ordering::Release);
        error(
            app,
            "Choose both an input folder and an output location from the File menu.",
        );
        return;
    };
    title(app, "drmr — starting");
    let handle = app.clone();
    let worker_state = state.clone();
    if std::thread::Builder::new().name("drmr-desktop-startup".into()).spawn(move || {
        let result = (|| {
            let cli = checked_cli().map_err(|_| ())?;
            let mut config = ServerConfig::new(catalog.clone(), output_parent.join("drmr-output"));
            config.common_config_root.clone_from(&private_config);
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
                let notices = handle.clone();
                let chooser = handle.clone();
                let directory = private_config.unwrap_or_else(|| output_parent.join("drmr-plans"));
                let exports = PlanExports::start(origin.clone(), &catalog, &output_parent.join("drmr-output"), &directory, move |notice| export_notice(&notices, notice), move |selection| choose_plan_file(&chooser, selection));
                let Ok(exports) = exports else {
                    failed_plan_start(&handle, &worker_state);
                    return;
                };
                *worker_state.exports.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(exports));
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
    let catalog = MenuItem::with_id(app, "catalog", "Choose input folder…", true, None::<&str>)?;
    let output = MenuItem::with_id(app, "output", "Choose output location…", true, None::<&str>)?;
    let private_config = MenuItem::with_id(
        app,
        "private-config",
        "Choose private configuration folder…",
        true,
        None::<&str>,
    )?;
    let clear_config = MenuItem::with_id(
        app,
        "clear-config",
        "Clear private configuration selection",
        true,
        None::<&str>,
    )?;
    let open = MenuItem::with_id(app, "open", "Open console", true, Some("CmdOrCtrl+O"))?;
    let tools = MenuItem::with_id(app, "tools", "Check external tools…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit drmr", true, Some("CmdOrCtrl+Q"))?;
    let application = Submenu::with_items(
        app,
        "drmr",
        true,
        &[&PredefinedMenuItem::about(app, None, None)?, &quit],
    )?;
    let file = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &catalog,
            &output,
            &private_config,
            &clear_config,
            &open,
            &tools,
        ],
    )?;
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
    if arguments.len() == 1 && arguments[0] == "--verify-bundle" {
        return if checked_cli().is_ok() {
            println!("bundle-cli-verified");
            std::process::ExitCode::SUCCESS
        } else {
            eprintln!("drmr-desktop: bundled-cli-mismatch");
            std::process::ExitCode::from(1)
        };
    }
    if arguments.len() == 1 && arguments[0] == "--version" {
        println!("drmr-desktop {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    if arguments.len() == 1 && arguments[0] == "--help" {
        println!(
            "drmr-desktop [--help | --version | --verify-bundle]\n\nOpen the native drmr console. Use the File menu to choose input and output folders. A separate private configuration folder for keys and fetch settings is optional."
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
            "catalog" => pick(app, FolderRole::Input),
            "output" => pick(app, FolderRole::Output),
            "private-config" => pick(app, FolderRole::PrivateConfig),
            "clear-config" => clear_private_config(app),
            "open" => open_console(app),
            "quit" => request_close(app),
            "tools" => diagnose_tools(app),
            _ => {}
        })
        .setup(move |app| {
            let links = app.handle().clone();
            let downloads = app.handle().clone();
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
                .on_download(move |_, event| native_download(&downloads, event))
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
    fn uncertain_diagnostic_cleanup_never_returns_to_idle_or_allows_exit() {
        let actual = [STARTING, CLOSING].map(|initial| {
            let phase = AtomicU8::new(initial);
            let completion = complete_diagnostics(&phase, false);
            (
                completion,
                phase.load(Ordering::Acquire),
                claim_close(&phase),
            )
        });
        assert_eq!(
            actual,
            [
                (
                    DiagnosticCompletion::Unconfirmed,
                    UNCONFIRMED,
                    Some(CloseAction::Shutdown {
                        previous: UNCONFIRMED
                    })
                ),
                (
                    DiagnosticCompletion::Unconfirmed,
                    UNCONFIRMED,
                    Some(CloseAction::Shutdown {
                        previous: UNCONFIRMED
                    })
                ),
            ]
        );
    }

    #[test]
    fn confirmed_diagnostics_show_results_or_finish_an_already_requested_close() {
        let phase = AtomicU8::new(STARTING);
        assert_eq!(
            complete_diagnostics(&phase, true),
            DiagnosticCompletion::Show
        );
        assert_eq!(phase.load(Ordering::Acquire), IDLE);
        phase.store(CLOSING, Ordering::Release);
        assert_eq!(
            complete_diagnostics(&phase, true),
            DiagnosticCompletion::Close
        );
        assert_eq!(phase.load(Ordering::Acquire), CLOSING);
    }

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
