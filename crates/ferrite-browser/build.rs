//! Windows-only: embed the brand icon into ferrite.exe so Explorer,
//! the taskbar, and the installer show the Ferrite mark instead of a
//! generic binary icon. No-op on every other target.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Rebuild when the icon changes: without this cargo considers the
    // build fresh and ships a stale icon (caught when the rounded tile
    // did not appear in the binary).
    println!("cargo:rerun-if-changed=../../brand/ferrite.ico");
    let mut res = winres::WindowsResource::new();
    res.set_icon("../../brand/ferrite.ico");
    if let Err(e) = res.compile() {
        panic!("embed brand/ferrite.ico into ferrite.exe: {e}");
    }
}
