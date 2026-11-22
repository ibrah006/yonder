// Gives yonder-agent.exe its icon (brand/yonder-icon*.svg, made into yonder.ico;
// see brand/README.md), for Explorer, Task Manager and the Services list.
fn main() {
    println!("cargo:rerun-if-changed=yonder.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("yonder.ico");
    // Cross-checks from a Mac have no resource compiler: build without the icon.
    if let Err(e) = resource.compile() {
        println!("cargo:warning=no icon embedded in yonder-agent.exe: {e}");
    }
}
