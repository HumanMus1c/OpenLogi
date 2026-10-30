use std::ffi::OsString;
use std::path::PathBuf;

use super::LaunchArgs;

/// Mirror of the overlay's guard: the `i18n!` in `main.rs` reaches the shared
/// catalog by relative path, and a wrong path does **not** fail the build:
/// `rust_i18n` compiles it to an empty catalog, and every tray string renders
/// as its semantic key. Resolve one tray key so that breakage is loud without
/// pinning any translation wording.
#[test]
fn the_shared_catalog_is_wired_up() {
    const KEY: &str = "app.show_main_window";
    assert_ne!(rust_i18n::t!(KEY), KEY);
}

#[test]
fn a_plain_launch_drives_real_devices() {
    let launch = LaunchArgs::parse(std::iter::empty()).expect("no arguments");

    assert!(!launch.mock);
    assert!(launch.demo_args().is_empty());
}

#[test]
fn the_mock_flag_selects_the_demo() {
    let launch = LaunchArgs::parse([OsString::from("--mock")].into_iter()).expect("--mock");

    assert!(launch.mock);
    assert!(launch.demo_args().is_empty());
}

#[test]
fn a_fixture_implies_the_demo_and_travels_to_it() {
    let launch = LaunchArgs::parse(
        [OsString::from("--fixture"), OsString::from("profile.json")].into_iter(),
    )
    .expect("--fixture profile.json");

    assert!(launch.mock, "a fixture is a demo profile");
    assert_eq!(launch.fixture, Some(PathBuf::from("profile.json")));
    assert_eq!(
        launch.demo_args(),
        [OsString::from("--fixture"), OsString::from("profile.json")]
    );
}

#[test]
fn an_unknown_argument_is_refused() {
    let error =
        LaunchArgs::parse([OsString::from("--demo")].into_iter()).expect_err("a typo is refused");

    assert!(error.contains("--demo"), "{error}");
}

#[test]
fn a_fixture_without_a_path_is_refused() {
    let error =
        LaunchArgs::parse([OsString::from("--fixture")].into_iter()).expect_err("a missing path");
    assert!(error.contains("profile JSON path"), "{error}");
}
