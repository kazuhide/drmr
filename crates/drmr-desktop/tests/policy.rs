use drmr_desktop::{ConsoleOrigin, bundled_cli};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use url::Url;

#[test]
fn only_an_owned_ipv4_loopback_listener_can_become_the_console() {
    for address in [
        "0.0.0.0:4000",
        "192.0.2.1:4000",
        "127.0.0.1:0",
        "[::1]:4000",
    ] {
        assert!(
            ConsoleOrigin::new(address.parse().unwrap()).is_none(),
            "{address}"
        );
    }
}

#[test]
fn the_console_keeps_paths_queries_and_media_on_its_exact_origin() {
    let origin = ConsoleOrigin::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 4000))).unwrap();
    for path in [
        "/",
        "/api/platform",
        "/assets/a/video.m3u8?preview=1",
        "/reports/report.html?view=full#checks",
    ] {
        assert!(origin.permits_navigation(&origin.url().join(path).unwrap()));
    }
}

#[test]
fn navigation_cannot_change_port_host_scheme_or_add_credentials() {
    let origin = ConsoleOrigin::new("127.0.0.1:4000".parse().unwrap()).unwrap();
    for value in [
        "http://127.0.0.1:4001/",
        "http://localhost:4000/",
        "http://127.0.0.2:4000/",
        "https://127.0.0.1:4000/",
        "http://name@127.0.0.1:4000/",
        "file:///tmp/index.html",
        "data:text/html,hello",
        "blob:http://127.0.0.1:4000/owned-fragment",
        "https://example.org/",
        "tauri://localhost/",
    ] {
        assert!(
            !origin.permits_navigation(&Url::parse(value).unwrap()),
            "{value}"
        );
    }
}

#[test]
fn plan_exports_require_the_exact_owned_endpoint_without_url_extras() {
    let origin = ConsoleOrigin::new("127.0.0.1:4000".parse().unwrap()).unwrap();
    let path = "/api/common/plans/0123456789abcdef0123456789abcdef/export";
    assert!(origin.permits_plan_export(&origin.url().join(path).unwrap()));
    for value in [
        format!("http://127.0.0.1:4001{path}"),
        format!("http://localhost:4000{path}"),
        format!("https://127.0.0.1:4000{path}"),
        format!("http://user@127.0.0.1:4000{path}"),
        format!("http://127.0.0.1:4000{path}?token=private"),
        format!("http://127.0.0.1:4000{path}#fragment"),
        format!("http://127.0.0.1:4000{path}/"),
        "http://127.0.0.1:4000/api/common/plans/0123456789ABCDEF0123456789ABCDEF/export".into(),
        "http://127.0.0.1:4000/api/common/plans/0123456789abcdef/export".into(),
        "http://127.0.0.1:4000/api/common/plans/%30123456789abcdef0123456789abcdef/export".into(),
        "http://127.0.0.1:4000/api//common/plans/0123456789abcdef0123456789abcdef/export".into(),
        "http://127.0.0.1:4000/assets/drmr-plan.json".into(),
    ] {
        assert!(
            !origin.permits_plan_export(&Url::parse(&value).unwrap()),
            "unexpected export admission: {value}"
        );
    }
}

#[test]
fn the_cli_is_an_absolute_sibling_without_search_path_fallback() {
    assert!(bundled_cli(Path::new("drmr-desktop")).is_none());
    let executable = std::env::current_exe().unwrap();
    let cli = bundled_cli(&executable).unwrap();
    assert!(cli.is_absolute());
    assert_eq!(cli.parent(), executable.parent());
    assert_eq!(
        cli.file_name().unwrap(),
        format!("drmr{}", std::env::consts::EXE_SUFFIX).as_str()
    );
}

#[test]
fn startup_navigation_accepts_the_real_tauri_index_alias_only() {
    for value in [
        "tauri://localhost",
        "tauri://localhost/",
        "tauri://localhost/index.html",
        "http://tauri.localhost/",
        "http://tauri.localhost/index.html",
    ] {
        assert!(
            drmr_desktop::permits_startup_navigation(&Url::parse(value).unwrap()),
            "{value}"
        );
    }
    for value in [
        "tauri://localhost/other",
        "tauri://localhost/?next=remote",
        "http://tauri.localhost:4000/",
        "http://user@tauri.localhost/",
        "http://127.0.0.1/",
        "https://example.org/",
        "file:///index.html",
    ] {
        assert!(
            !drmr_desktop::permits_startup_navigation(&Url::parse(value).unwrap()),
            "{value}"
        );
    }
}
